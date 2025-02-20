use std::fs::File;
use std::io::Write;
use std::path::{self, Path};
use std::process::Command;
use std::{env, io};

pub async fn build_css(
    output_dir: &Path,
    assets_dir: &Path,
    download_tailwind: bool,
    major_version: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    let binary_path = if download_tailwind {
        download_tailwind_binary(assets_dir, major_version).await?
    } else {
        tailwind_path()?
    };

    eprintln!("Building CSS with Tailwind: {binary_path}");
    let output = Command::new(binary_path)
        .args([
            "--input",
            assets_dir.join("main.css").to_str().unwrap(),
            "--output",
            Path::new("./assets").join("dist.css").to_str().unwrap(),
            "--cwd",
            output_dir.to_str().unwrap(),
            "--minify",
        ])
        .output()?;
    std::io::stderr().write_all(&output.stderr)?;
    if !output.status.success() {
        panic!(
            "Tailwind failed to compile {}",
            assets_dir.join("main.css").display()
        );
    }

    Ok(())
}

/// Returns the path to execute the Tailwind binary.
///
/// If a `tailwindcss` binary already exists on the current path (determined
/// using `tailwindcss --help`), then the existing Tailwind is used. Otherwise,
/// a Tailwind binary is installed from GitHub releases into the user's cache
/// directory.
fn tailwind_path() -> Result<String, Box<dyn std::error::Error>> {
    let result = Command::new("tailwindcss").arg("--help").status();
    match result {
        Ok(status) if status.success() => Ok("tailwindcss".to_owned()),
        _ => Err("Could not find Tailwind binary".into()),
    }
}

pub async fn download_tailwind_binary(
    binary_path: &path::Path,
    major_version: u8,
) -> Result<String, Box<dyn std::error::Error>> {
    let double = match (env::consts::OS, env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("linux", "arm") => "linux-armv7",
        ("macos", "x86_64") => "macos-x64",
        ("macos", "aarch64") => "macos-arm64",
        ("windows", "x86_64") => "windows-x64.exe",
        ("windows", "aarch64") => "windows-arm64.exe",
        _ => "linux-x64",
    };
    let binary_path = binary_path.join("tailwindcss");
    // tested versions that i'm sure will work
    let version_tag = match major_version {
        4 => "v4.0.7",
        _ => return Err(format!("Unsupported Tailwind major version: {major_version}").into()),
    };
    if !binary_path.exists() {
        eprintln!("Downloading Tailwind {version_tag} binary to {binary_path:?}");
        let url = format!(
            "https://github.com/tailwindlabs/tailwindcss/releases/download/{version_tag}/tailwindcss-{double}"
        );
        let response = reqwest::get(&url).await?;
        if response.status().is_success() {
            let mut file = File::create(&binary_path)?;
            let mut content = io::Cursor::new(response.bytes().await?);
            io::copy(&mut content, &mut file)?;

            // On non-Windows platforms, we need to mark the file as executable
            #[cfg(target_family = "unix")]
            {
                use std::os::unix::prelude::PermissionsExt;
                let user_execute = std::fs::Permissions::from_mode(0o755);
                std::fs::set_permissions(&binary_path, user_execute)?;
            }
        } else {
            return Err(format!(
                "Failed to download Tailwind {version_tag} for {double}: {}",
                response.status()
            )
            .into());
        }
    } else {
        eprintln!("Tailwind binary already exists at {binary_path:?}");
    }

    Ok(binary_path.to_str().unwrap().to_owned())
}
