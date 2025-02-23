use crate::assets::css::build_css;
use crate::provider::{Insertable, OnlineProvider, ProviderPocket};
use chrono_tz::Tz;
use clap::Parser;
use cli::{
    AuthArgs, CliArgs, FetchArgs, LocalAddArgs, LocalCommands, LocalFavoriteArgs, NotesArgs,
    PocketAddArgs, PocketCommands, PocketFavoriteArgs, Subcommands,
};
use db::{ResearchItem, Tags, DB};
use provider::local::LocalItem;
use site::Site;
use sqlx::migrate::MigrateDatabase;
use std::env;
use std::path::Path;
use std::str::FromStr;
use tokio::fs::{create_dir, metadata, read_to_string, File};
use tokio::io::AsyncWriteExt;
use util::absolute_path;

mod assets;
mod cli;
mod db;
mod handler;
mod provider;
mod site;
mod util;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli_args = CliArgs::parse();

    match &cli_args.subcommand {
        Some(Subcommands::Pocket { command }) => {
            handle_pocket_command(command, &cli_args).await?
        }
        Some(Subcommands::Local { command }) => {
            handle_local_command(command, &cli_args).await?
        }
        Some(Subcommands::Fetch { limit }) => handle_fetch_command(&cli_args, *limit).await?,
        Some(Subcommands::List {
            tags,
            limit,
            favorite_only,
            timezone,
        }) => {
            let favorite = if *favorite_only { Some(true) } else { None };
            let timezone = timezone
                .as_ref()
                .and_then(|tz_str| Tz::from_str(tz_str).ok());
            handle_list_command(&cli_args, tags.as_ref(), favorite, *limit, timezone).await?
        }
        Some(Subcommands::Init { path }) => handle_init_command(path, &cli_args).await?,
        Some(Subcommands::Generate {
            output,
            assets,
            download_tailwind,
            timezone,
        }) => {
            let timezone = timezone
                .as_ref()
                .and_then(|tz_str| Tz::from_str(tz_str).ok());
            handle_generate_command(output, assets, *download_tailwind, timezone, &cli_args)
                .await?
        }
        Some(Subcommands::Export { raindrop, output }) => {
            if *raindrop {
                let db = DB::init(&cli_args.db).await.map_err(handle_db_error)?;

                if output == "-" {
                    db.export_to_csv(None).await?;
                } else {
                    db.export_to_csv(Some(output)).await?;
                    println!("Exported to {output}");
                }
            }
        }
        Some(Subcommands::Handle {
            register,
            unregister,
            url,
        }) => {
            if *register {
                handler::platform_register_url();
            } else if *unregister {
                handler::platform_unregister_url();
            } else if let Some(url) = url {
                handler::handle_url(url).await?;
            }
        }
        Some(Subcommands::Notes(NotesArgs { url, notes })) => {
            let db = DB::init(&cli_args.db).await.map_err(handle_db_error)?;
            if db.get_item_id(&url).await?.is_none() {
                eprintln!("Item with URL {} not found in the database", url);
                return Ok(());
            }
            db.update_notes(&url, &notes).await?;
            println!("Notes updated successfully!");
        }
        None => {
            eprintln!("No subcommand provided");
            eprintln!("Please provide a subcommand");
            eprintln!("Run with --help for more information");
        }
    }
    Ok(())
}

async fn handle_local_command(
    command: &LocalCommands,
    cli_args: &CliArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let db = DB::init(&cli_args.db).await.map_err(handle_db_error)?;
    let provider_id = db.get_provider_id("local").await?;
    match command {
        LocalCommands::Add(LocalAddArgs {
            uri,
            tag,
            title,
            excerpt,
        }) => {
            println!("{uri} {title:?} {excerpt:?} {tag:?}");
            let tags: Vec<Tags> = tag.as_ref().map_or(Vec::new(), |tags| {
                tags.iter()
                    .map(|tag| Tags {
                        tag_name: tag.clone(),
                    })
                    .collect()
            });

            let metadata = handler::fetch_metadata(uri).await?;
            let local_item = LocalItem {
                id: None,
                uri: uri.to_string(),
                title: Some(title.clone().map_or(metadata.title, |title| title.clone())),
                excerpt: Some(
                    excerpt
                        .clone()
                        .map_or(metadata.description, |excerpt| excerpt.clone()),
                ),
                time_added: chrono::Utc::now().timestamp(),
                tags: tags.clone(),
            };

            db.insert_item(local_item.to_research_item(), &tags, provider_id)
                .await?;
            println!("Inserted document successfully!");
        }
        LocalCommands::List => {
            let items = db.get_all_items_by_provider(provider_id).await?;
            println!("Items: {:?}", items.len());
            for item in items {
                println!("{:?}", item);
            }
        }
        LocalCommands::Favorite(LocalFavoriteArgs { uri, mark }) => {
            let item_id = db
                .get_item_id(uri)
                .await?
                .expect("Item uri not found in the database");
            db.mark_as_favorite(item_id, *mark).await?;
            println!("Item marked as favorite: {mark}");
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
            let db = DB::init(&cli_args.db).await.map_err(handle_db_error)?;

            let provider = ProviderPocket {
                consumer_key: key.to_string(),
                ..Default::default()
            };
            let secrets = provider.authenticate().await?;
            db.set_secret(secrets).await?;
            println!("Success: Access token saved to the database! You can now run `pocket fetch` to fetch items from Pocket.")
        }
        PocketCommands::Fetch(FetchArgs { key, access, limit }) => {
            // Handle fetching items from Pocket with the provided keys
            fetch_from_pocket(&cli_args.db, key.to_string(), access.to_string(), *limit)
                .await?;
        }
        PocketCommands::Add(PocketAddArgs {
            add_args: LocalAddArgs { uri, tag, .. },
            key,
            access,
        }) => {
            // Handle adding an item to Pocket with the provided URI and tags
            let db = DB::init(&cli_args.db).await.map_err(handle_db_error)?;
            let secrets = db.get_secrets().await?;
            let consumer_key = secrets.pocket_consumer_key.or(key.clone()).expect(
                "Consumer key not found in the database, consider generating one from https://getpocket.com/developer/apps/new and running `pocket auth`",
            );
            let access_token = secrets.pocket_access_token.or(access.clone()).expect(
                "Access token not found in the database, consider running 'pocket auth'",
            );
            let provider = ProviderPocket {
                consumer_key,
                access_token: Some(access_token),
                ..Default::default()
            };
            let item_id = provider
                .add_item(
                    uri,
                    tag.as_ref().map_or(Vec::new(), |tags| {
                        tags.iter().map(|tag| tag.as_str()).collect()
                    }),
                )
                .await?;

            // add item to db
            let tags: Vec<Tags> = tag.as_ref().map_or(Vec::new(), |tags| {
                tags.iter()
                    .map(|tag| Tags {
                        tag_name: tag.clone(),
                    })
                    .collect()
            });

            println!("Saving item to database");
            let metadata = handler::fetch_metadata(uri).await?;
            let insertable_item = ResearchItem {
                id: item_id,
                uri: uri.to_string(),
                title: metadata.title,
                excerpt: metadata.description,
                time_added: chrono::Utc::now().timestamp(),
                favorite: false,
                lang: None,
                notes: None,
            };
            let provider_id = db.get_provider_id("pocket").await?;
            println!("Item: {insertable_item:?}");
            db.insert_item(insertable_item, &tags, provider_id).await?;
        }
        PocketCommands::Favorite(PocketFavoriteArgs {
            fav_args:
                LocalFavoriteArgs {
                    uri,
                    mark: favorite,
                },
            access,
            key,
        }) => {
            // Handle adding an item to Pocket with the provided URI and tags
            let db = DB::init(&cli_args.db).await.map_err(handle_db_error)?;
            let secrets = db.get_secrets().await?;
            let consumer_key = secrets.pocket_consumer_key.or(key.clone()).expect(
                "Consumer key not found in the database, consider generating one from https://getpocket.com/developer/apps/new and running `pocket auth`",
            );
            let access_token = secrets.pocket_access_token.or(access.clone()).expect(
                "Access token not found in the database, consider running 'pocket auth'",
            );
            let provider = ProviderPocket {
                consumer_key,
                access_token: Some(access_token),
                ..Default::default()
            };
            let item_id = db
                .get_item_id(uri)
                .await?
                .expect("Item uri not found in the database");
            provider.mark_as_favorite(item_id, *favorite).await?;
            db.mark_as_favorite(item_id, *favorite).await?;
            println!("Item marked as favorite: {favorite}");
        }
    }
    Ok(())
}

async fn handle_fetch_command(
    cli_args: &CliArgs,
    limit: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Handle fetching data from authenticated providers
    let db = DB::init(&cli_args.db).await.map_err(handle_db_error)?;
    let secrets = db.get_secrets().await?;
    let consumer_key = secrets.pocket_consumer_key.expect("Consumer key not found in the database, consider generating one from https://getpocket.com/developer/apps/new and running `pocket auth`");
    let access_token = secrets
        .pocket_access_token
        .expect("Access token not found in the database, consider running 'pocket auth'");
    fetch_from_pocket(&cli_args.db, consumer_key, access_token, limit).await?;
    Ok(())
}

async fn handle_list_command(
    cli_args: &CliArgs,
    tags: Option<&Vec<String>>,
    favorite: Option<bool>,
    limit: Option<usize>,
    timezone: Option<Tz>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Handle listing items in the database
    let db = DB::init(&cli_args.db).await.map_err(handle_db_error)?;
    let mut items: Vec<ResearchItem>;
    if let Some(tags) = tags {
        items = db.get_all_items_by_tags(tags, favorite).await?;
        println!("Tags: {:?}", tags);
        println!("Total items: {}", items.len());
        if let Some(limit) = limit {
            items.truncate(limit);
        }
    } else {
        items = db.get_all_items(favorite).await?;
        println!("Total items: {}", items.len());
        if let Some(limit) = limit {
            items.truncate(limit);
        }
    }
    println!("Displaying {} items:", items.len());
    for item in items {
        println!("Research Item");
        println!("-------------");
        if let Some(id) = item.id {
            println!("ID: {}", id);
        }
        println!("{}", item.to_display_with_timezone(timezone));
    }
    Ok(())
}

async fn handle_init_command(
    db_path: &str,
    _cli_args: &CliArgs,
) -> Result<(), Box<dyn std::error::Error>> {
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
    timezone: Option<Tz>,
    cli_args: &CliArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    // Handle generating a static site with the provided options
    metadata(&assets_dir)
        .await
        .unwrap_or_else(|_| panic!("Invalid assets directory: {assets_dir}"));
    const REQUIRED_FILES: [&str; 2] = ["main.css", "search.js"];
    for file in REQUIRED_FILES {
        metadata(&Path::new(&assets_dir).join(file))
            .await
            .unwrap_or_else(|_| panic!("Missing required file in assets directory: {file}"));
    }

    let output_dir = Path::new(output_dir);
    if !output_dir.exists() {
        create_dir(output_dir).await?;
    }

    let db = DB::init(&cli_args.db).await.map_err(handle_db_error)?;
    let tags = db.get_all_tags().await?;
    let item_tags = db.get_all_item_tags().await?;

    let site = Site::build(&tags, &item_tags, "./assets", timezone)?;

    eprintln!("Output directory: {output_dir:?}");
    let mut index = File::create(output_dir.join("index.html")).await?;
    index.write_all(site.index_html.as_bytes()).await?;

    let mut search = File::create(output_dir.join("search.html")).await?;
    search.write_all(site.search_html.as_bytes()).await?;

    build_css(
        output_dir,
        &absolute_path(
            env::current_dir().expect("Failed to get current directory"),
            Path::new(assets_dir),
        ),
        download_tailwind,
        4,
    )
    .await?;

    let search_js = Path::new(assets_dir).join("search.js");
    let mut search = File::create(output_dir.join("assets").join("search.js")).await?;
    search
        .write_all(read_to_string(&search_js).await?.as_bytes())
        .await?;

    Ok(())
}

async fn fetch_from_pocket(
    db_url: &str,
    consumer_key: String,
    access_token: String,
    limit: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    let provider = ProviderPocket {
        consumer_key,
        access_token: Some(access_token),
        ..Default::default()
    };

    let db = DB::init(db_url).await?;
    println!("Sqlite version: {}", db.get_sqlite_version().await?);

    let provider_id = db.get_provider_id("pocket").await?;
    let items = provider.fetch_items(limit).await?;
    eprintln!("Items: {}", items.len());

    for item in items {
        let insertable_item = item.to_research_item();
        let tags = item.to_tags();
        let title = &insertable_item.title.chars().take(8).collect::<String>();
        eprint!("{:?} ", title);
        db.insert_item(insertable_item, &tags, provider_id).await?;
    }

    Ok(())
}

fn handle_db_error(err: sqlx::Error) -> sqlx::Error {
    match &err {
        sqlx::Error::Database(dberr) => {
            if dberr.message().contains("no such column") {
                eprintln!("Database schema needs updating. Please run:");
                eprintln!("pocket-research init <path-to-db>");
                eprintln!("\nThis will apply all pending migrations.");
            } else {
                eprintln!("Database error: {}", dberr);
            }
        }
        sqlx::Error::PoolTimedOut => {
            eprintln!("Database connection timed out");
        }
        _ => {
            eprintln!("Unknown database error: {}", err);
        }
    }
    err
}
