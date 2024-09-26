use std::env;
use std::path::PathBuf;
use std::process::Command;

pub fn platform_unregister_url() {
    println!("Unregistering URL handler for the research:// protocol");

    #[cfg(target_os = "windows")]
    unregister_windows();

    #[cfg(target_os = "macos")]
    unregister_macos();

    #[cfg(target_os = "linux")]
    unregister_linux();

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    println!("Unsupported operating system");
}

#[cfg(target_os = "windows")]
fn unregister_windows() {
    let reg_command = r#"REG DELETE "HKCU\Software\Classes\research" /f"#;

    Command::new("cmd")
        .args(&["/C", reg_command])
        .output()
        .expect("Failed to execute registry command");

    println!("URL handler unregistered for Windows");
}

#[cfg(target_os = "macos")]
fn unregister_macos() {
    let app_name = "ResearchURLHandler.app";
    let home_dir = env::var("HOME").unwrap();
    let app_path = PathBuf::from(&home_dir).join("Applications").join(app_name);

    std::fs::remove_dir_all(app_path).unwrap_or_else(|e| {
        println!("Failed to remove app bundle: {}", e);
    });

    println!("URL handler unregistered for macOS");
}

#[cfg(target_os = "linux")]
fn unregister_linux() {
    let home_dir = env::var("HOME").unwrap();
    let desktop_file_path =
        PathBuf::from(&home_dir).join(".local/share/applications/research-url-handler.desktop");

    std::fs::remove_file(desktop_file_path).unwrap_or_else(|e| {
        println!("Failed to remove desktop file: {}", e);
    });

    Command::new("xdg-mime")
        .args(["uninstall", "research-url-handler.desktop"])
        .output()
        .expect("Failed to unregister MIME type");

    println!("URL handler unregistered for Linux");
}
