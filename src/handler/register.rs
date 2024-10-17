use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

pub fn platform_register_url() {
    #[cfg(target_os = "windows")]
    register_windows();
    #[cfg(target_os = "macos")]
    register_macos();
    #[cfg(target_os = "linux")]
    register_linux();

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    println!("Unsupported operating system");
}

#[cfg(target_os = "windows")]
fn register_windows() {
    let executable_path = env::current_exe().unwrap();
    let executable_path_str = executable_path.to_str().unwrap();
    let reg_cmd = format!("{} handle --url \"%1\"", executable_path_str);
    let commands = vec![
        vec!["REG", "ADD", "HKEY_CLASSES_ROOT\\research", "/ve", "/d", "Research Pocket Url Handler", "/f"],
        vec!["REG", "ADD", "HKEY_CLASSES_ROOT\\research\\shell", "/f"],
        vec!["REG", "ADD", "HKEY_CLASSES_ROOT\\research\\shell\\open", "/f"],
        vec![
            "REG",
            "ADD",
            "HKEY_CLASSES_ROOT\\research\\shell\\open\\command",
            "/ve",
            "/d",
            &reg_cmd,
            "/f",
        ],
    ];

    for command in commands {
        let output = Command::new("cmd")
            .args(["/C"])
            .args(command.clone())
            .output()
            .expect("Failed to execute command");

        if output.status.success() {
            println!("Successfully executed: {:?}", command);
        } else {
            panic!(
                "Failed to execute: {:?}. Error: {}",
                command,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[cfg(target_os = "windows")]
    {
        use winrt_notification::{Duration, Toast};
        Toast::new(Toast::POWERSHELL_APP_ID)
            .title("ResearchPocket Handler")
            .text1("Handler registered!")
            .duration(Duration::Short)
            .show()
            .expect("Failed to send notification");
    }
    println!("URL handler registered for Windows");
}

#[cfg(target_os = "macos")]
fn register_macos() {
    let app_name = "ResearchURLHandler.app";
    let home_dir = env::var("HOME").unwrap();
    let app_path = PathBuf::from(&home_dir).join("Applications").join(app_name);

    std::fs::create_dir_all(&app_path).unwrap();
    std::fs::create_dir_all(app_path.join("Contents/MacOS")).unwrap();

    let executable_path = env::current_exe().unwrap();
    std::fs::copy(
        &executable_path,
        app_path.join("Contents/MacOS/ResearchURLHandler"),
    )
    .unwrap();

    let plist_content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>ResearchURLHandler</string>
    <key>CFBundleIdentifier</key>
    <string>com.example.ResearchURLHandler</string>
    <key>CFBundleName</key>
    <string>ResearchURLHandler</string>
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key>
            <string>Research URL</string>
            <key>CFBundleURLSchemes</key>
            <array>
                <string>research</string>
            </array>
        </dict>
    </array>
    <key>CFBundleExecutable</key>
    <string>{}</string>
</dict>
</plist>"#,
        executable_path.file_name().unwrap().to_str().unwrap()
    );

    let mut file = File::create(app_path.join("Contents/Info.plist")).unwrap();
    file.write_all(plist_content.as_bytes()).unwrap();

    println!("URL handler registered for macOS");
}

#[cfg(target_os = "linux")]
fn register_linux() {
    let desktop_entry = format!(
        r#"[Desktop Entry]
Type=Application
Name=Research URL Handler
Exec={} handle --url %u
StartupNotify=false
MimeType=x-scheme-handler/research;"#,
        env::current_exe().unwrap().to_str().unwrap()
    );

    let home_dir = env::var("HOME").unwrap();
    let apps_dir = PathBuf::from(&home_dir).join(".local/share/applications");
    std::fs::create_dir_all(&apps_dir).unwrap();

    let desktop_file_path = apps_dir.join("research-url-handler.desktop");
    let mut file = File::create(desktop_file_path).unwrap();
    file.write_all(desktop_entry.as_bytes()).unwrap();

    Command::new("xdg-mime")
        .args([
            "default",
            "research-url-handler.desktop",
            "x-scheme-handler/research",
        ])
        .output()
        .expect("Failed to register MIME type");

    println!("URL handler registered for Linux");
}
