use clap::{arg, command, Command};
use providers::pocket::{self, PocketItem};
use sqlx::{migrate::MigrateDatabase, FromRow, Row};

mod providers;
mod util;

const DB_URL: &str = "sqlite:research.sqlite";

#[derive(Clone, FromRow, Debug)]
#[allow(dead_code)]
pub struct Providers {
    id: i64,
    name: String,
}

#[derive(Clone, FromRow, Debug, sqlx::Type)]
#[allow(dead_code)]
pub struct Tags {
    tag_name: String,
}

#[derive(Clone, FromRow, Debug)]
pub struct ResearchItem {
    id: i64,
    uri: String,
    title: String,
    excerpt: String,
    time_added: i64,
    favorite: bool,
    lang: Option<String>,
}

pub trait Insertable {
    fn to_research_item(&self) -> ResearchItem;
    fn to_tags(&self) -> Vec<Tags>;
}

pub trait Provider {
    type Item: Insertable;

    fn authenticate(
        &self,
    ) -> impl std::future::Future<Output = Result<(), Box<dyn std::error::Error>>> + Send;

    fn fetch_items(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<Self::Item>, Box<dyn std::error::Error>>> + Send;
}

#[derive(Debug, Default)]
pub struct ProviderPocket {
    // provider-specific fields
    consumer_key: String,
    access_token: Option<String>,
    client: reqwest::Client,
}

impl Provider for ProviderPocket {
    type Item = PocketItem;

    async fn authenticate(&self) -> Result<(), Box<dyn std::error::Error>> {
        // authenticate with the Pocket API
        pocket::login(&self.client, &self.consumer_key).await?;
        Ok(())
    }

    async fn fetch_items(&self) -> Result<Vec<Self::Item>, Box<dyn std::error::Error>> {
        // fetch items from the Pocket API
        let access_token = self.access_token.as_ref().ok_or("Access token not found")?;
        pocket::get(&access_token, &self.consumer_key, &self.client)
            .await
            .map(|items| items.to_vec())
    }
}

impl Insertable for PocketItem {
    fn to_research_item(&self) -> ResearchItem {
        let title = if !self.given_title.is_empty() {
            Some(self.given_title.clone())
        } else if !self.resolved_title.clone().unwrap_or_default().is_empty() {
            self.resolved_title.clone()
        } else {
            Some("<No Title>".to_string())
        }
        .unwrap();

        let uri = self
            .given_url
            .as_ref()
            .or(self.resolved_url.as_ref())
            .map_or("#".into(), |url| url.to_string());

        ResearchItem {
            id: self.item_id as i64,
            uri,
            title,
            excerpt: self.excerpt.as_ref().map_or("".to_string(), |s| s.clone()),
            time_added: self.time_added.timestamp(),
            favorite: self.favorite,
            lang: self.lang.clone(),
        }
    }
    fn to_tags(&self) -> Vec<Tags> {
        self.tags.as_ref().map_or(vec![], |tags| {
            tags.iter()
                .map(|tag| Tags {
                    tag_name: tag.tag.clone(),
                })
                .collect()
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = command!() // requires `cargo` feature
        .arg(arg!(-d --debug ... "Turn debugging information on"))
        .subcommand(
            Command::new("login")
                .about("does login things")
                .arg(arg!([token])),
        )
        .subcommand(Command::new("fetch").about("Gets all pocket data"))
        .subcommand(Command::new("test").about("tests sqlite"))
        .subcommand(Command::new("list").about("Lists all items in sqlite"))
        .get_matches();

    if let Some(matches) = matches.subcommand_matches("login") {
        let consumer_key = matches
            .get_one::<String>("token")
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                std::env::var("POCKET_CONSUMER_KEY").expect("A consumer_key is required")
            });

        println!("token: {consumer_key:?}");
        let client = reqwest::Client::new();
        pocket::login(&client, &consumer_key).await?;
    }

    if let Some(_) = matches.subcommand_matches("fetch") {
        let consumer_key = std::env::var("POCKET_CONSUMER_KEY").expect("Required consumer key");
        let access_token = std::env::var("POCKET_ACCESS_TOKEN").expect("required access token");
        let client = reqwest::Client::new();
        pocket::get(&access_token, &consumer_key, &client).await?;
    }

    if let Some(_) = matches.subcommand_matches("test") {
        let pool = sqlx::SqlitePool::connect(DB_URL).await;
        match pool {
            Ok(pool) => {
                let row = sqlx::query("SELECT sqlite_version()")
                    .fetch_one(&pool)
                    .await?;
                println!("Sqlite version: {}", row.get::<String, _>(0));

                let provider_id = sqlx::query_as::<_, Providers>(
                    "SELECT * FROM providers WHERE name = 'pocket'",
                )
                .fetch_one(&pool)
                .await?;

                eprintln!("Provider id: {}", provider_id.id);

                let provider = ProviderPocket {
                    consumer_key: std::env::var("POCKET_CONSUMER_KEY")
                        .expect("Required consumer key"),
                    access_token: Some(
                        std::env::var("POCKET_ACCESS_TOKEN").expect("required access token"),
                    ),
                    client: reqwest::Client::new(),
                };

                let items = provider.fetch_items().await?;
                eprintln!("Items: {}", items.len());

                for item in items {
                    let insertable_item = item.to_research_item();
                    let _ = sqlx::query(
                        "INSERT INTO items (id, uri, title, excerpt, time_added, favorite, lang, provider_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    )
                    .bind(insertable_item.id)
                    .bind(insertable_item.uri)
                    .bind(insertable_item.title)
                    .bind(insertable_item.excerpt)
                    .bind(insertable_item.time_added)
                    .bind(insertable_item.favorite)
                    .bind(insertable_item.lang)
                    .bind(provider_id.id)
                    .execute(&pool).await?;

                    let tags = item.to_tags();
                    for tag in tags {
                        let _ = sqlx::query("INSERT OR IGNORE INTO tags (tag_name) VALUES (?)")
                            .bind(tag.tag_name.clone())
                            .execute(&pool)
                            .await?;
                        let _ = sqlx::query(
                            "INSERT INTO item_tags (item_id, tag_name) VALUES (?, ?)",
                        )
                        .bind(insertable_item.id)
                        .bind(tag.tag_name)
                        .execute(&pool)
                        .await?;
                    }
                }
            }

            Err(sqlx::Error::Database(err)) => {
                eprintln!("Database error: {err}");
                eprintln!("Creating new database: {DB_URL}");
                sqlx::Sqlite::create_database(DB_URL).await?;
                let pool = sqlx::SqlitePool::connect(DB_URL).await?;
                let _ = sqlx::migrate!("./migrations").run(&pool).await;
                // get the provider id for pocket
                let provider_id = sqlx::query_as::<_, Providers>(
                    "SELECT * FROM providers WHERE name = 'pocket'",
                )
                .fetch_one(&pool)
                .await?;

                eprintln!("Provider id: {}", provider_id.id);
            }
            Err(err) => {
                eprintln!("Unknown error: {err}");
            }
        }
    }

    if let Some(_) = matches.subcommand_matches("list") {
        let pool = sqlx::SqlitePool::connect(DB_URL).await?;
        let items = sqlx::query_as::<_, ResearchItem>("SELECT * FROM items")
            .fetch_all(&pool)
            .await?;
        for item in items {
            println!("{:?}", item);
        }
    }

    Ok(())
}
