mod db;
mod provider;
mod util;

use crate::provider::{Insertable, Provider, ProviderPocket};
use clap::{arg, command, Command};
use db::DB;
use sqlx::migrate::MigrateDatabase;
use util::env::Env;

const DB_URL: &str = "sqlite:research.sqlite";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = command!() // requires `cargo` feature
        .arg(arg!(-d --debug ... "Turn debugging information on"))
        .subcommand(
            Command::new("pocket")
                .about("Pocket related actions")
                .subcommand(
                    Command::new("auth")
                        .about("Authenticate using a consumer key")
                        .arg(arg!(--key <CONSUMER_KEY> "Required consumer key")),
                )
                .subcommand(
                    Command::new("fetch")
                        .about("fetch items from pocket")
                        .args(&[
                            arg!(--key <CONSUMER_KEY> "Pocket Consumer key"),
                            arg!(--access <ACCESS_TOKEN> "Pocket Access token"),
                        ]),
                ),
        )
        .subcommand(Command::new("fetch").about("Gets all data from authenticated providers"))
        .subcommand(Command::new("list").about("Lists all items in db"))
        .get_matches();

    let env = Env::new();

    if let Some(matches) = matches.subcommand_matches("pocket") {
        if let Some(matches) = matches.subcommand_matches("auth") {
            let consumer_key = matches
                .get_one::<String>("key")
                .map(|s| s.to_string())
                .unwrap_or_else(|| env.pocket_consumer_key.expect("Required consumer key"));

            println!("token: {consumer_key:?}");
            let provider = ProviderPocket {
                consumer_key,
                ..Default::default()
            };
            provider.authenticate().await?;
        } else if let Some(matches) = matches.subcommand_matches("fetch") {
            let consumer_key = matches
                .get_one::<String>("key")
                .map(|s| s.to_string())
                .unwrap_or_else(|| env.pocket_consumer_key.expect("Required consumer key"));
            let access_token = matches
                .get_one::<String>("access")
                .map(|s| s.to_string())
                .unwrap_or_else(|| env.pocket_access_token.expect("Required access token"));

            let provider = ProviderPocket {
                consumer_key,
                access_token: Some(access_token),
                ..Default::default()
            };

            match DB::init(&DB_URL).await {
                Ok(db) => {
                    println!("Sqlite version: {}", db.get_sqlite_version().await?);

                    let provider_id = db.get_provider_id("pocket").await?;

                    eprintln!("Provider id: {}", provider_id);

                    let items = provider.fetch_items().await?;
                    eprintln!("Items: {}", items.len());

                    for item in items {
                        let insertable_item = item.to_research_item();
                        let tags = item.to_tags();
                        eprintln!("Inserting item: {:?}", insertable_item.title);
                        db.insert_item(insertable_item, tags, provider_id).await?;
                    }
                }

                Err(sqlx::Error::Database(err)) => {
                    eprintln!("Database error: {err}");
                    eprintln!("First run :D\nCreating new database: {DB_URL}");
                    sqlx::Sqlite::create_database(DB_URL).await?;
                    let pool = sqlx::SqlitePool::connect(DB_URL).await?;
                    DB::migrate(&pool).await?;
                }

                Err(err) => {
                    eprintln!("Unknown error: {err}");
                }
            }
        }
    } else if let Some(_) = matches.subcommand_matches("fetch") {
        unimplemented!()
    }

    if let Some(_) = matches.subcommand_matches("list") {
        let db = DB::init(&DB_URL).await?;
        let items = db.get_all_items().await?;
        for item in items {
            println!("{:?}", item);
        }
    }

    Ok(())
}
