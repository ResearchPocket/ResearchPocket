mod register;
mod unregister;

use scraper::{Html, Selector};
use std::process::Command;

pub use register::platform_register_url;
pub use unregister::platform_unregister_url;
use url::Url;

use crate::{
    db::{Tags, DB},
    provider::{local::LocalItem, Insertable},
};

pub async fn handle_url(url: &str) -> Result<(), sqlx::Error> {
    match Url::parse(url) {
        Ok(parsed_url) if parsed_url.scheme() == "research" => {
            return handle_research_url(parsed_url).await
        }
        Ok(_) => println!("Not a research URL"),
        Err(e) => println!("Invalid URL: {}", e),
    }
    Ok(())
}

#[derive(Debug)]
struct WebpageMetadata {
    pub title: String,
    pub description: String,
}

async fn fetch_metadata(url: &str) -> Result<WebpageMetadata, reqwest::Error> {
    // Make the HTTP request
    let response = reqwest::get(url).await?;
    let html_content = response.text().await?;

    // Parse the HTML
    let document = Html::parse_document(&html_content);

    // Extract title
    let title_selector = Selector::parse("title").unwrap();
    let title = document
        .select(&title_selector)
        .next()
        .and_then(|el| el.text().next())
        .unwrap_or("")
        .to_string();

    // Extract description
    let description_selector = Selector::parse("meta[name='description']").unwrap();
    let description = document
        .select(&description_selector)
        .next()
        .and_then(|el| el.value().attr("content"))
        .unwrap_or("")
        .to_string();

    Ok(WebpageMetadata { title, description })
}

/// the url looks like research://save?url=https%3A%2F%2Fwww.rust-lang.org&provider=local&tags=rust,programming&db_path=/path/to/db
async fn handle_research_url(parsed_url: Url) -> Result<(), sqlx::Error> {
    println!("Handling research URL: {}", parsed_url);
    let query_params: Vec<(String, String)> = parsed_url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    println!("Extracted query parameters: {:?}", query_params);

    let url = query_params
        .iter()
        .find(|(k, _)| k == "url")
        .map(|(_, v)| v)
        .ok_or_else(|| sqlx::Error::Protocol("Missing URL parameter".into()))?;
    let provider = query_params
        .iter()
        .find(|(k, _)| k == "provider")
        .map(|(_, v)| v);
    let tags = query_params
        .iter()
        .find(|(k, _)| k == "tags")
        .map(|(_, v)| v.split(',').collect::<Vec<_>>());
    let db_path = query_params
        .iter()
        .find(|(k, _)| k == "db_path")
        .map(|(_, v)| v);

    println!("URL: {:?}", url);
    println!("Provider: {:?}", provider);
    println!("Tags: {:?}", tags);
    println!("Database path: {:?}", db_path);

    let db = DB::init(db_path.unwrap_or(&"research.db".to_string()))
        .await
        .map_err(|err| {
            match err {
                sqlx::Error::Database(..) => {
                    eprintln!("Database not found \"{:?}\" or research.db", db_path);
                    eprintln!("Please set the database corrdct path with --db");
                    eprintln!("Or consider initializing the database with the 'init' command");
                }
                _ => {
                    eprintln!("Unknown error: {err:?}");
                }
            }
            err
        })?;

    let provider_id = db
        .get_provider_id(provider.unwrap_or(&"local".to_string()))
        .await
        .expect("Failed to get provider id");
    println!("Provider ID: {:?}", provider_id);

    let tags: Vec<Tags> = tags
        .unwrap_or_default()
        .iter()
        .map(|t| Tags {
            tag_name: t.to_string(),
        })
        .collect();

    // Fetch metadata from the URL
    let metadata = fetch_metadata(url)
        .await
        .map_err(|e| sqlx::Error::Protocol(format!("Failed to fetch metadata: {}", e)))?;
    println!("Metadata: {:?}", metadata);

    let local_item = LocalItem {
        id: None,
        uri: url.to_string(),
        title: Some(metadata.title),
        excerpt: Some(metadata.description),
        time_added: chrono::Utc::now().timestamp(),
        tags: tags.clone(),
    };

    println!("Inserting item into database");
    let result = db
        .insert_item(local_item.to_research_item(), &tags, provider_id)
        .await;

    let (title, message) = match result {
        Ok(_) => (
            "Research URL Handler - Success",
            format!(
                "Successfully saved:\n{}\nTags: {} {}",
                url.chars().take(50).collect::<String>(),
                tags.iter()
                    .map(|t| t.tag_name.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
                provider.unwrap_or(&"None".to_string())
            ),
        ),
        Err(e) => (
            "Research URL Handler - Error",
            format!("Failed to save:\n{}\nError: {}", url, e),
        ),
    };

    #[cfg(target_os = "linux")]
    {
        Command::new("notify-send")
            .args([&title, &message.as_ref()])
            .output()
            .expect("Failed to send notification");
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("osascript")
            .args([
                "-e",
                &format!(
                    "display notification \"{}\" with title \"{}\"",
                    message, title
                ),
            ])
            .output()
            .expect("Failed to send notification");
    }

    #[cfg(target_os = "windows")]
    {
        use winrt_notification::{Duration, Toast};
        Toast::new(Toast::POWERSHELL_APP_ID)
            .title(&title)
            .text1(&message)
            .duration(Duration::Short)
            .show()
            .expect("Failed to send notification");
    }

    println!("Item inserted successfully");
    Ok(())
}
