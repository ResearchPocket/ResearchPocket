mod register;
mod unregister;

use reqwest::header::CONTENT_TYPE;
use scraper::{Html, Selector};
use std::path::Path;
use std::process::Command;

pub use register::platform_register_url;
pub use unregister::platform_unregister_url;
use url::Url;

use crate::{
    db::{Tags, DB},
    provider::{local::LocalItem, Insertable, OnlineProvider, ProviderPocket},
};

pub async fn handle_url(url: &str) -> Result<(), sqlx::Error> {
    match Url::parse(url) {
        Ok(parsed_url) if parsed_url.scheme() == "research" => {
            let res = handle_research_url(parsed_url).await;
            if let Err(e) = res {
                #[cfg(target_os = "linux")]
                {
                    Command::new("notify-send")
                        .args([&format!("{}", e), "Handler - Error"])
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
                                e, "Handler - Error"
                            ),
                        ])
                        .output()
                        .expect("Failed to send notification");
                }

                #[cfg(target_os = "windows")]
                {
                    use winrt_notification::{Duration, Toast};
                    Toast::new(Toast::POWERSHELL_APP_ID)
                        .title("Handler - Error")
                        .text1(&format!("{}", e))
                        .duration(Duration::Short)
                        .show()
                        .expect("Failed to send notification");
                }
                return Err(e);
            }
        }
        Ok(_) => println!("Not a research URL"),
        Err(e) => println!("Invalid URL: {}", e),
    }
    Ok(())
}

#[derive(Debug)]
pub struct WebpageMetadata {
    pub title: String,
    pub description: String,
}

pub async fn fetch_metadata(url: &str) -> Result<WebpageMetadata, Box<dyn std::error::Error>> {
    // Make the HTTP request
    let response = reqwest::get(url).await?;

    // Get the content type
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|ct| ct.to_str().ok())
        .unwrap_or("");

    // Handle different content types
    if content_type.starts_with("text/html") {
        // For HTML pages, use the existing logic
        let html_content = response.text().await?;
        let document = Html::parse_document(&html_content);

        let title = extract_title(&document);
        let description = extract_description(&document);

        Ok(WebpageMetadata { title, description })
    } else {
        // For non-HTML content, use the URL's filename and MIME type
        let file_name = Path::new(url)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("");
        let mime_type = content_type.split(';').next().unwrap_or("");

        Ok(WebpageMetadata {
            title: file_name.to_string(),
            description: format!("File type: {}", mime_type),
        })
    }
}

fn extract_title(document: &Html) -> String {
    let title_selector = Selector::parse("title").unwrap();
    document
        .select(&title_selector)
        .next()
        .and_then(|el| el.text().next())
        .unwrap_or("")
        .to_string()
}

fn extract_description(document: &Html) -> String {
    let description_selector = Selector::parse("meta[name='description']").unwrap();
    document
        .select(&description_selector)
        .next()
        .and_then(|el| el.value().attr("content"))
        .unwrap_or("")
        .to_string()
}

/// the url looks like research://save?url=https%3A%2F%2Fwww.rust-lang.org&provider=local&tags=rust,programming&db_path=/path/to/db
async fn handle_research_url(parsed_url: Url) -> Result<(), sqlx::Error> {
    let query_params: Vec<(String, String)> = parsed_url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

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
        .expect(format!("Failed to get provider ID for {:?}", provider).as_str());
    println!("Provider ID: {:?}", provider_id);

    // Fetch metadata from the URL
    let metadata = fetch_metadata(url)
        .await
        .map_err(|e| sqlx::Error::Protocol(format!("Failed to fetch metadata: {}", e)))?;
    println!("Metadata: {:?}", metadata);

    let mut id: Option<i64> = None;

    match provider {
        Some(p) => match p.as_str() {
            "local" => {}
            "pocket" => {
                let secrets = db.get_secrets().await?;
                let consumer_key = secrets
                    .pocket_consumer_key
                    .expect("Missing pocket consumer key");
                let access_token = secrets
                    .pocket_access_token
                    .expect("Missing pocket access token");
                let provider = ProviderPocket {
                    consumer_key,
                    access_token: Some(access_token),
                    ..Default::default()
                };
                id = provider
                    .add_item(url, tags.clone().unwrap_or_default())
                    .await
                    .map_err(|e| {
                        eprintln!("Failed to add item to pocket: {}", e);
                        sqlx::Error::Protocol("Failed to add item to pocket".into())
                    })?;
            }
            _ => {
                eprintln!("Provider \"{:?}\" not supported", p);
                return Err(sqlx::Error::Protocol("Provider not supported".into()));
            }
        },
        None => {
            eprintln!("Provider not specified using default provider \"local\"");
        }
    }

    let tags: Vec<Tags> = tags
        .unwrap_or_default()
        .iter()
        .map(|t| Tags {
            tag_name: t.to_string(),
        })
        .collect();

    let local_item = LocalItem {
        id,
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
        Err(e) => {
            println!("Failed to save item: {}", e);
            (
                "Research URL Handler - Error",
                format!("Failed to save:\n{}\nError: {}", url, e),
            )
        }
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
