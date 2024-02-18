use std::fs::File;
use std::io::Write;
use std::path::{self, Path};
use std::process::Command;
use std::{env, io};

const TAILWIND_CONFIG_FILE: &str = "tailwind.config.js";
pub const DEFAULT_CSS_OUTPUT_FILE: &str = "dist.css";

pub async fn build_css(
    input_css_file: &Path,
    download_tailwind: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let binary_path = {
        let tailwind_path = tailwind_path();
        if tailwind_path.is_ok() {
            tailwind_path
        } else if download_tailwind {
            download_tailwind_binary(path::Path::new(input_css_file).parent().unwrap()).await
        } else {
            tailwind_path
        }
    }?;

    eprintln!("Building CSS with Tailwind: {binary_path}");

    let input_css_path = input_css_file.parent().expect("Invalid CSS file");
    let tailwind_config_path = input_css_path.join(TAILWIND_CONFIG_FILE);
    let output_path = input_css_path.join(DEFAULT_CSS_OUTPUT_FILE);
    let output = Command::new(binary_path)
        .args([
            "-c",
            tailwind_config_path.to_str().unwrap(),
            "-i",
            input_css_file.to_str().unwrap(),
            "-o",
            output_path.to_str().unwrap(),
            "--minify",
        ])
        .output()?;
    std::io::stderr().write_all(&output.stderr)?;
    output.status.success().then_some(true).unwrap_or_else(|| {
        panic!("Tailwind failed to compile {input_css_file:?} to {output_path:?}")
    });

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
    if !binary_path.exists() {
        eprintln!("Downloading Tailwind binary to {binary_path:?}");
        let url = format!("https://github.com/tailwindlabs/tailwindcss/releases/latest/download/tailwindcss-{double}");
        let response = reqwest::get(url).await?;
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
        eprintln!("Tailwind binary already exists at {binary_path:?}");
    }

    Ok(binary_path.to_str().unwrap().to_owned())
}
