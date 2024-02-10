use crate::assets::css::build_css;
use crate::provider::{Insertable, Provider, ProviderPocket};
use clap::{arg, command, Command};
use db::DB;
use site::Site;
use sqlx::migrate::MigrateDatabase;
use std::path::Path;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

mod db;
mod provider;
mod site;
mod util;
mod assets;

const DB_URL: &str = "sqlite:research.sqlite";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = command!() // requires `cargo` feature
        .arg(arg!(-d --debug ... "Turn debugging information on"))
        .author("KorigamiK <korigamik>")
        .subcommand(
            Command::new("pocket")
                .about("Pocket related actions")
                .subcommand(
                    Command::new("auth")
                        .about("Authenticate using a consumer key")
                        .arg(
                            arg!(-k --key <CONSUMER_KEY> "Consumer key (https://getpocket.com/developer/apps/new)")
                                .env("POCKET_CONSUMER_KEY")
                                .required(true)
                        ),
                )
                .subcommand(
                    Command::new("fetch")
                        .about("Fetch items from pocket")
                        .args(&[
                            arg!(--key <CONSUMER_KEY> "Pocket Consumer key")
                                .env("POCKET_CONSUMER_KEY")
                                .required(true),
                            arg!(--access <ACCESS_TOKEN> "Pocket Access token")
                                .env("POCKET_ACCESS_TOKEN")
                                .required(true),
                        ]),
                ),
        )
        .subcommand(Command::new("fetch").about("Gets all data from authenticated providers"))
        .subcommand(Command::new("list").about("Lists all items in db"))
        .subcommand(
            Command::new("generate")
                .about("Generate a static site")
                .arg(arg!(path: [PATH]).index(1)),
        )
        .get_matches();

    if let Some(matches) = matches.subcommand_matches("pocket") {
        if let Some(matches) = matches.subcommand_matches("auth") {
            let consumer_key = matches
                .get_one::<String>("key")
                .expect("Required consumer key")
                .to_string();

            println!("consumer_key: {consumer_key:?}");
            let provider = ProviderPocket {
                consumer_key,
                ..Default::default()
            };
            provider.authenticate().await?;
        } else if let Some(matches) = matches.subcommand_matches("fetch") {
            let consumer_key = matches
                .get_one::<String>("key")
                .expect("Required consumer key")
                .to_string();
            let access_token = matches
                .get_one::<String>("access")
                .expect("Required access token")
                .to_string();

            let provider = ProviderPocket {
                consumer_key,
                access_token: Some(access_token),
                ..Default::default()
            };

            match DB::init(DB_URL).await {
                Ok(db) => {
                    println!("Sqlite version: {}", db.get_sqlite_version().await?);

                    let provider_id = db.get_provider_id("pocket").await?;
                    let items = provider.fetch_items().await?;
                    eprintln!("Items: {}", items.len());

                    for item in items {
                        let insertable_item = item.to_research_item();
                        let tags = item.to_tags();
                        let title = &insertable_item.title.chars().take(8).collect::<String>();
                        eprint!("{:?} ", title);
                        db.insert_item(insertable_item, tags, provider_id).await?;
                    }
                }

                Err(sqlx::Error::Database(err)) => {
                    eprintln!("Database error: {err}");
                    eprintln!("First run :D\nCreating new database: {DB_URL}");
                    sqlx::Sqlite::create_database(DB_URL).await?;
                    let pool = sqlx::SqlitePool::connect(DB_URL).await?;
                    DB::migrate(&pool).await?;
                    eprintln!("You should run the command again")
                }

                Err(err) => {
                    eprintln!("Unknown error: {err}");
                }
            }
        }
    } else if matches.subcommand_matches("fetch").is_some() {
        unimplemented!()
    } else if matches.subcommand_matches("list").is_some() {
        let db = DB::init(DB_URL).await?;
        let items = db.get_all_items().await?;
        for item in items {
            println!("{:?}", item);
        }
    } else if let Some(matches) = matches.subcommand_matches("generate") {
        let db = DB::init(DB_URL).await?;
        let tags = db.get_all_tags().await?;
        let item_tags = db.get_all_item_tags().await?;
        let site = Site::build(&tags, &item_tags)?;
        let site_path = matches
            .get_one::<String>("path")
            .expect("Path to the site is required");
        let site_path = Path::new(&site_path);

        build_css(site_path)?;

        eprintln!("Site path: {site_path:?}");
        let mut index = File::create(site_path.join("index.html")).await?;
        index.write_all(site.index_html.as_bytes()).await?;

        let mut search = File::create(site_path.join("search.html")).await?;
        search.write_all(site.search_html.as_bytes()).await?;
    }

    Ok(())
}
