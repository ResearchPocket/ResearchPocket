use std::io::Write;
use std::path::Path;
use std::process::Command;

const TAILWIND_CONFIG_FILE: &str = "tailwind.config.js";
pub const DEFAULT_CSS_OUTPUT_FILE: &str = "dist.css";

pub fn build_css(input_css_file: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let binary_path = tailwind_path()?;

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
    // First, see if tailwind is already present
    let result = Command::new("tailwindcss").arg("--help").status();
    if let Ok(status) = result {
        if status.success() {
            return Ok("tailwindcss".to_owned());
        }
        eprintln!("Couldn't find Tailwind binary");
        Err("Could not find Tailwind binary")?
        // Otherwise, no tailwind binary exists.
    } else {
        eprintln!("Couldn't find Tailwind binary");
        Err("Could not find Tailwind binary")?
    }
}
