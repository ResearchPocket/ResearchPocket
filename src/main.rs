use crate::assets::css::build_css;
use crate::provider::{Insertable, Provider, ProviderPocket};
use clap::Parser;
use cli::{AuthArgs, CliArgs, FetchArgs, PocketCommands, Subcommands};
use db::DB;
use site::Site;
use sqlx::migrate::MigrateDatabase;
use std::path::Path;
use tokio::fs::{metadata, File};
use tokio::io::AsyncWriteExt;

mod assets;
mod cli;
mod db;
mod provider;
mod site;
mod util;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli_args = CliArgs::parse();

    match &cli_args.subcommand {
        Some(Subcommands::Pocket {
            command: pocket_command,
        }) => handle_pocket_command(pocket_command, &cli_args).await?,
        Some(Subcommands::Fetch) => handle_fetch_command(&cli_args).await?,
        Some(Subcommands::List) => handle_list_command(&cli_args).await?,
        Some(Subcommands::Init { path }) => handle_init_command(path, &cli_args).await?,
        Some(Subcommands::Generate {
            output,
            assets,
            download_tailwind,
        }) => handle_generate_command(output, assets, *download_tailwind, &cli_args).await?,
        None => {
            eprintln!("No subcommand provided");
            eprintln!("Please provide a subcommand");
            eprintln!("Run with --help for more information");
        }
    }

    Ok(())
}

async fn handle_pocket_command(
    pocket_command: &PocketCommands,
    cli_args: &CliArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    match pocket_command {
        PocketCommands::Auth(AuthArgs { key }) => {
            // Handle authentication with the provided consumer key
            let db = DB::init(&cli_args.db).await.map_err(|err| {
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

            println!("consumer_key: {key:?}");
            let provider = ProviderPocket {
                consumer_key: key.to_string(),
                ..Default::default()
            };
            let secrets = provider.authenticate().await?;
            db.set_secret(secrets).await?;
        }
        PocketCommands::Fetch(FetchArgs { key, access }) => {
            // Handle fetching items from Pocket with the provided keys
            fetch_from_pocket(&cli_args.db, key.to_string(), access.to_string()).await?;
        }
    }
    Ok(())
}

async fn handle_fetch_command(cli_args: &CliArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Handle fetching data from authenticated providers
    let db = DB::init(&cli_args.db).await.map_err(|err| {
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
    let consumer_key = secrets.pocket_consumer_key.expect("Consumer key not found in the database, consider generating one from https://getpocket.com/developer/apps/new and running `pocket auth`");
    let access_token = secrets
        .pocket_access_token
        .expect("Access token not found in the database, consider running 'pocket auth'");
    fetch_from_pocket(&cli_args.db, consumer_key, access_token).await?;
    Ok(())
}

async fn handle_list_command(cli_args: &CliArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Handle listing items in the database
    let db = DB::init(&cli_args.db).await.map_err(|err| {
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
    Ok(())
}

async fn handle_init_command(
    path: &str,
    _cli_args: &CliArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    // Handle initializing the database at the provided path
    let db_path = path;
    let db_url = {
        let path = Path::new(&db_path).join("research.sqlite");
        path.to_str().expect("Invalid db path").to_owned()
    };
    eprintln!("Creating new database: {db_url}");
    sqlx::Sqlite::create_database(&db_url).await?;
    let pool = sqlx::SqlitePool::connect(&db_url).await?;
    DB::migrate(&pool).await?;
    eprintln!("Database created and migrated successfully!");
    Ok(())
}

async fn handle_generate_command(
    output: &str,
    assets: &str,
    download_tailwind: bool,
    cli_args: &CliArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    // Handle generating a static site with the provided options
    let site_path = output;
    let site_path = Path::new(&site_path);

    let assets_dir = assets;
    let assets_dir = site_path.join(assets_dir);

    {
        let dir = site_path.join(assets_dir.clone());
        metadata(&dir)
            .await
            .unwrap_or_else(|_| panic!("Invalid assets directory: {:?}", dir));
    }

    let assets_dir = assets_dir.to_str().unwrap().to_owned();

    let db = DB::init(&cli_args.db).await.map_err(|err| {
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

    build_css(input_css, download_tailwind).await?;

    eprintln!("Site path: {site_path:?}");
    let mut index = File::create(site_path.join("index.html")).await?;
    index.write_all(site.index_html.as_bytes()).await?;

    let mut search = File::create(site_path.join("search.html")).await?;
    search.write_all(site.search_html.as_bytes()).await?;
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
