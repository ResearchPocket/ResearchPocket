use std::env;
use std::io::Write;
use std::path::Path;
use std::process::Command;

const MANIFEST_PATH: &str = env!("CARGO_MANIFEST_DIR");
const CSS_SRC_PATH: &str = "../css/main.css";
const TAILWIND_SRC_PATH: &str = "../tailwind.config.js";
const DEFAULT_CSS_OUTPUT_DIR: &str = "./dist.css";

pub fn build_css(dist_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let binary_path = tailwind_path()?;

    let manifest_dir = Path::new(MANIFEST_PATH);
    let css_src_path = manifest_dir.join(CSS_SRC_PATH);
    let tailwind_config_path = manifest_dir.join(TAILWIND_SRC_PATH);
    let output_path = dist_dir.join(DEFAULT_CSS_OUTPUT_DIR);
    let output = Command::new(binary_path)
        .args([
            "-c",
            tailwind_config_path.to_str().unwrap(),
            "-i",
            css_src_path.to_str().unwrap(),
            "-o",
            output_path.to_str().unwrap(),
            "--minify",
        ])
        .output()?;
    std::io::stderr().write_all(&output.stderr)?;
    output
        .status
        .success()
        .then_some(true)
        .expect("Tailwind failed to compile CSS!");

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
