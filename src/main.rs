use crate::assets::css::build_css;
use crate::provider::{Insertable, Provider, ProviderPocket};
use clap::{arg, command, crate_authors, crate_description, crate_name, Command};
use db::DB;
use site::Site;
use sqlx::migrate::MigrateDatabase;
use std::path::Path;
use tokio::fs::{metadata, File};
use tokio::io::AsyncWriteExt;

mod assets;
mod db;
mod provider;
mod site;
mod util;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = command!() // requires `cargo` feature
        .before_help(format!("{} 🔖", crate_name!().to_uppercase()))
        .author(crate_authors!("\n"))
        .about(crate_description!())
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
        .subcommand(Command::new("list").about("Lists all items in the database"))
        .subcommand(Command::new("init")
            .about("Initializes the database")
            .arg(arg!(path: [PATH] "This path will be used to create the database file and SAVED for future use.")
                .index(1)
                .required(true)))
        .subcommand(
            Command::new("generate")
                .about("Generate a static site")
                .args(&[
                    arg!(output: [PATH] "The path to the output directory").index(1).required(true),
                    arg!(--assets <ASSETS_DIR> "Path to site assets (main.css, search.js) RELATIVE to the output directory")
                        .default_value("./assets"),
                    clap::Arg::new("tailwind")
                        .long("download-tailwind")
                        .help("Download Tailwind binary to <ASSETS_DIR>/tailwindcss if not found")
                        .action(clap::ArgAction::SetTrue)
                ]),
        )
        .args(&[
            arg!(--db <DB_URL> "Database url")
                .env("DATABASE_URL")
                .default_value("./research.sqlite"),
            arg!(-d --debug ... "Turn debugging information on")
        ])
        .arg_required_else_help(true)
        .get_matches();

    let db_url = matches.get_one::<String>("db").expect("Database url");

    if let Some(matches) = matches.subcommand_matches("pocket") {
        if let Some(matches) = matches.subcommand_matches("auth") {
            let db = DB::init(db_url).await.map_err(|err| {
                match err {
                    sqlx::Error::Database(..) => {
                        eprintln!("Database not found");
                        eprintln!("Please set the database corrdct path with --db");
                        eprintln!(
                            "Or consider initializing the database with the 'init' command"
                        );
                    }
                    _ => {
                        eprintln!("Unknown error: {err:?}");
                    }
                }
                err
            })?;
            let consumer_key = matches
                .get_one::<String>("key")
                .expect("Required consumer key")
                .to_string();

            println!("consumer_key: {consumer_key:?}");
            let provider = ProviderPocket {
                consumer_key,
                ..Default::default()
            };
            let secrets = provider.authenticate().await?;
            db.set_secret(secrets).await?;
        } else if let Some(matches) = matches.subcommand_matches("fetch") {
            let consumer_key = matches
                .get_one::<String>("key")
                .expect("Required consumer key")
                .to_string();
            let access_token = matches
                .get_one::<String>("access")
                .expect("Required access token")
                .to_string();
            fetch_from_pocket(db_url, consumer_key, access_token).await?;
        }
    } else if matches.subcommand_matches("fetch").is_some() {
        let db = DB::init(db_url).await.map_err(|err| {
            match err {
                sqlx::Error::Database(..) => {
                    eprintln!("Database not found");
                    eprintln!("Please set the database corrdct path with --db");
                    eprintln!("Or consider initializing the database with the 'init' command");
                }
                _ => {
                    eprintln!("Unknown error: {err:?}");
                }
            }
            err
        })?;
        let secrets = db.get_secrets().await?;
        let consumer_key = secrets.pocket_consumer_key.expect("Consumer key not found in the database, consider generating one from https://getpocket.com/developer/apps/new and run 'pocket auth'");
        let access_token = secrets
            .pocket_access_token
            .expect("Access token not found in the database, consider running 'pocket auth'");
        fetch_from_pocket(db_url, consumer_key, access_token).await?;
    } else if matches.subcommand_matches("list").is_some() {
        let db = DB::init(db_url).await.map_err(|err| {
            match err {
                sqlx::Error::Database(..) => {
                    eprintln!("Database not found");
                    eprintln!("Please set the database corrdct path with --db");
                    eprintln!("Or consider initializing the database with the 'init' command");
                }
                _ => {
                    eprintln!("Unknown error: {err:?}");
                }
            }
            err
        })?;
        let items = db.get_all_items().await?;
        for item in items {
            println!("{:?}", item);
        }
    } else if let Some(matches) = matches.subcommand_matches("generate") {
        let site_path = matches.get_one::<String>("output").unwrap();
        let site_path = Path::new(&site_path);

        let assets_dir = matches.get_one::<String>("assets").unwrap();
        let assets_dir = site_path.join(assets_dir);

        {
            let dir = site_path.join(assets_dir.clone());
            metadata(&dir)
                .await
                .unwrap_or_else(|_| panic!("Invalid assets directory: {:?}", dir));
        }

        let assets_dir = assets_dir.to_str().unwrap().to_owned();

        let db = DB::init(db_url).await.map_err(|err| {
            eprintln!("Please set the corrdct database path with --db");
            err
        })?;
        let tags = db.get_all_tags().await?;
        let item_tags = db.get_all_item_tags().await?;

        let site = Site::build(&tags, &item_tags, &assets_dir)?;

        metadata(&Path::new(&assets_dir).join("search.js"))
            .await
            .expect("Missing search.js");

        let input_css = &Path::new(&assets_dir).join("main.css");
        metadata(input_css).await.expect("Missing main.css");

        let download_tailwind = matches.get_flag("tailwind");
        build_css(input_css, download_tailwind).await?;

        eprintln!("Site path: {site_path:?}");
        let mut index = File::create(site_path.join("index.html")).await?;
        index.write_all(site.index_html.as_bytes()).await?;

        let mut search = File::create(site_path.join("search.html")).await?;
        search.write_all(site.search_html.as_bytes()).await?;
    } else if let Some(matches) = matches.subcommand_matches("init") {
        let db_path = matches.get_one::<String>("path").expect("Database path");
        let db_url = {
            let path = Path::new(&db_path).join("research.sqlite");
            path.to_str().expect("Invalid db path").to_owned()
        };
        eprintln!("Creating new database: {db_url}");
        sqlx::Sqlite::create_database(&db_url).await?;
        let pool = sqlx::SqlitePool::connect(&db_url).await?;
        DB::migrate(&pool).await?;
        eprintln!("Database created and migrated successfully!");
    }

    Ok(())
}

async fn fetch_from_pocket<'a>(
    db_url: &str,
    consumer_key: String,
    access_token: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let provider = ProviderPocket {
        consumer_key,
        access_token: Some(access_token),
        ..Default::default()
    };

    let db = DB::init(db_url).await?;
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

    Ok(())
}
