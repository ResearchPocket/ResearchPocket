mod register;
mod unregister;

use std::process::Command;

pub use register::platform_register_url;
pub use unregister::platform_unregister_url;
use url::Url;

pub fn handle_url(url: &str) {
    match Url::parse(url) {
        Ok(parsed_url) if parsed_url.scheme() == "research" => handle_research_url(parsed_url),
        Ok(_) => println!("Not a research URL"),
        Err(e) => println!("Invalid URL: {}", e),
    }
}

/// the url looks like research://save?url=https%3A%2F%2Fwww.rust-lang.org&provider=local&tags=rust,programming
fn handle_research_url(parsed_url: Url) {
    let query_params: Vec<(String, String)> = parsed_url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let url = query_params
        .iter()
        .find(|(k, _)| k == "url")
        .map(|(_, v)| v);
    let provider = query_params
        .iter()
        .find(|(k, _)| k == "provider")
        .map(|(_, v)| v);
    let tags = query_params
        .iter()
        .find(|(k, _)| k == "tags")
        .map(|(_, v)| v.split(',').collect::<Vec<_>>());

    println!("URL: {:?}", url);
    println!("Provider: {:?}", provider);
    println!("Tags: {:?}", tags);

    #[cfg(target_os = "linux")]
    {
        let summary = format!(
            "{}\nTags: {} {}",
            url.unwrap_or(&"None".to_string())
                .chars()
                .take(50)
                .collect::<String>(),
            tags.map(|t| t.join(", ")).unwrap_or("None".to_string()),
            provider.unwrap_or(&"None".to_string()),
        );
        Command::new("notify-send")
            .args(&["Research URL Handler", &summary])
            .output()
            .expect("Failed to send notification");
    }
}
