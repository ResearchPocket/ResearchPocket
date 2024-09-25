use crate::assets::css::build_css;
use crate::provider::{Insertable, Provider, ProviderPocket};
use clap::Parser;
use cli::{AuthArgs, CliArgs, FetchArgs, PocketCommands, Subcommands};
use db::DB;
use site::Site;
use sqlx::migrate::MigrateDatabase;
use std::path::Path;
use tokio::fs::{create_dir, metadata, read_to_string, File};
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
        Some(Subcommands::List { tag }) => handle_list_command(&cli_args, tag.as_ref()).await?,
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
        Some(Subcommands::Export { raindrop }) => {
            if *raindrop {
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
                db.export_to_csv("raindrop_export.csv").await?;
                println!("Exported to raindrop_export.csv");
            }
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

            let provider = ProviderPocket {
                consumer_key: key.to_string(),
                ..Default::default()
            };
            let secrets = provider.authenticate().await?;
            db.set_secret(secrets).await?;
            println!("Success: Access token saved to the database! You can now run `pocket fetch` to fetch items from Pocket.")
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

async fn handle_list_command(
    cli_args: &CliArgs,
    tags: Option<&Vec<String>>,
) -> Result<(), Box<dyn std::error::Error>> {
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
    if let Some(tags) = tags {
        let items = db.get_items_by_tags(tags).await?;
        println!("Tags: {:?}", tags);
        println!("Items: {:?}", items.len());
        for item in items {
            println!("{:?}", item);
        }
    } else {
        let items = db.get_all_items().await?;
        println!("Items: {:?}", items.len());
        for item in items {
            println!("{:?}", item);
        }
    }
    Ok(())
}

async fn handle_init_command(
    db_path: &str,
    _cli_args: &CliArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    // Handle initializing the database at the provided path
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
    output_dir: &str,
    assets_dir: &str,
    download_tailwind: bool,
    cli_args: &CliArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    // Handle generating a static site with the provided options
    metadata(&assets_dir)
        .await
        .unwrap_or_else(|_| panic!("Invalid assets directory: {assets_dir}"));
    const REQUIRED_FILES: [&str; 3] = ["main.css", "search.js", "tailwind.config.js"];
    for file in REQUIRED_FILES {
        metadata(&Path::new(&assets_dir).join(file))
            .await
            .unwrap_or_else(|_| panic!("Missing required file: {file}"));
    }

    let output_dir = Path::new(output_dir);
    if !output_dir.exists() {
        create_dir(output_dir).await?;
    }

    let db = DB::init(&cli_args.db).await.map_err(|err| {
        eprintln!("Please set the corrdct database path with --db");
        err
    })?;
    let tags = db.get_all_tags().await?;
    let item_tags = db.get_all_item_tags().await?;

    let site = Site::build(&tags, &item_tags, "./assets")?;

    eprintln!("Output directory: {output_dir:?}");
    let mut index = File::create(output_dir.join("index.html")).await?;
    index.write_all(site.index_html.as_bytes()).await?;

    let mut search = File::create(output_dir.join("search.html")).await?;
    search.write_all(site.search_html.as_bytes()).await?;

    build_css(
        &Path::new(assets_dir).join("main.css"),
        &Path::new(assets_dir).join("tailwind.config.js"),
        &Path::new(output_dir).join("assets").join("dist.css"),
        download_tailwind,
    )
    .await?;

    let search_js = Path::new(assets_dir).join("search.js");
    let mut search = File::create(output_dir.join("assets").join("search.js")).await?;
    search
        .write_all(read_to_string(&search_js).await?.as_bytes())
        .await?;

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
