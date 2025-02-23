use std::{
    error::Error,
    fmt::{self, Display},
};

use chrono::{DateTime, Local, TimeZone, Utc};
use chrono_tz::Tz;
use csv::WriterBuilder;
use serde::Serialize;
use sqlx::{sqlite::SqlitePoolOptions, FromRow, Pool, Row, Sqlite};
use std::fs::File;
use std::io;

#[derive(Clone, FromRow, Debug)]
#[allow(dead_code)]
pub struct Providers {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, FromRow, Debug, sqlx::Type, Serialize)]
pub struct Tags {
    pub tag_name: String,
}

#[derive(Clone, FromRow, Debug, Serialize)]
pub struct ResearchItem {
    pub id: Option<i64>,
    pub uri: String,
    pub title: String,
    pub excerpt: String,
    pub time_added: i64,
    pub favorite: bool,
    pub lang: Option<String>,
}

impl fmt::Display for ResearchItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Research Item")?;
        writeln!(f, "-------------")?;
        if let Some(id) = self.id {
            writeln!(f, "ID: {}", id)?;
        }
        writeln!(f, "{}", self.to_display_with_timezone(None))
    }
}

impl ResearchItem {
    /// Of the format "21 Aug'21, 5pm"
    pub fn format_time_added(&self, timezone: Option<Tz>) -> String {
        let utc_datetime = Utc.timestamp_opt(self.time_added, 0).unwrap();
        fn format_datetime<Tz: TimeZone>(datetime: DateTime<Tz>) -> String
        where
            Tz::Offset: Display,
        {
            datetime.format("%d %b'%y, %l%P").to_string()
        }

        match timezone {
            Some(tz) => format_datetime(utc_datetime.with_timezone(&tz)),
            None => format_datetime(utc_datetime.with_timezone(&Local)),
        }
    }

    pub fn to_display_with_timezone(&self, timezone: Option<Tz>) -> String {
        let time_added = self.format_time_added(timezone);
        format!(
            "Title: {}\nURL: {}\nAdded: {}\nFavorite: {}\nLanguage: {}\nExcerpt:\n{}",
            self.title,
            self.uri,
            time_added,
            if self.favorite { "Yes" } else { "No" },
            self.lang.as_ref().unwrap_or(&"Unknown".to_string()),
            self.excerpt
        )
    }
}

#[derive(FromRow, Default)]
#[allow(dead_code)]
pub struct Secrets {
    pub pocket_consumer_key: Option<String>,
    pub pocket_access_token: Option<String>,
    pub user_id: i64,
}

impl Secrets {
    pub fn new(s: Option<Self>) -> Self {
        s.unwrap_or_default()
    }
}

const DEFAULT_USER_ID: i64 = 0;

pub struct DB {
    pub pool: Pool<Sqlite>,
}

impl DB {
    pub async fn init(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = SqlitePoolOptions::new().connect(database_url).await?;
        Ok(Self { pool })
    }

    pub async fn migrate(pool: &Pool<Sqlite>) -> Result<(), sqlx::Error> {
        let _ = sqlx::migrate!("./migrations").run(pool).await;
        Ok(())
    }

    pub async fn get_sqlite_version(&self) -> Result<String, sqlx::Error> {
        let row = sqlx::query("SELECT sqlite_version()")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<String, _>(0))
    }

    pub async fn get_provider_id(&self, name: &str) -> Result<i64, sqlx::Error> {
        let provider = sqlx::query_as::<_, Providers>("SELECT * FROM providers WHERE name = ?")
            .bind(name)
            .fetch_one(&self.pool)
            .await?;
        Ok(provider.id)
    }

    pub async fn insert_item(
        &self,
        insertable_item: ResearchItem,
        tags: &[Tags],
        provider_id: i64,
    ) -> Result<(), sqlx::Error> {
        let _ = sqlx::query(
                        "INSERT OR IGNORE INTO items (id, uri, title, excerpt, time_added, favorite, lang, provider_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    )
                    .bind(insertable_item.id)
                    .bind(insertable_item.uri)
                    .bind(insertable_item.title)
                    .bind(insertable_item.excerpt)
                    .bind(insertable_item.time_added)
                    .bind(insertable_item.favorite)
                    .bind(insertable_item.lang)
                    .bind(provider_id)
                    .execute(&self.pool).await?;

        for tag in tags {
            let _ = sqlx::query("INSERT OR IGNORE INTO tags (tag_name) VALUES (?)")
                .bind(tag.tag_name.clone())
                .execute(&self.pool)
                .await?;
            let _ = sqlx::query(
                "INSERT OR IGNORE INTO item_tags (item_id, tag_name) VALUES (?, ?)",
            )
            .bind(insertable_item.id)
            .bind(&tag.tag_name)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn get_all_items(
        &self,
        favorite: Option<bool>,
    ) -> Result<Vec<ResearchItem>, sqlx::Error> {
        let query = match favorite {
            Some(true) => "SELECT * FROM items WHERE favorite = 1 ORDER BY time_added DESC",
            Some(false) => "SELECT * FROM items WHERE favorite = 0 ORDER BY time_added DESC",
            None => "SELECT * FROM items ORDER BY time_added DESC",
        };

        sqlx::query_as::<_, ResearchItem>(query)
            .fetch_all(&self.pool)
            .await
    }

    pub async fn get_all_items_by_tags(
        &self,
        tags: &[String],
        favorite: Option<bool>,
    ) -> Result<Vec<ResearchItem>, sqlx::Error> {
        let favorite_clause = match favorite {
            Some(true) => "AND items.favorite = 1",
            Some(false) => "AND items.favorite = 0",
            None => "",
        };

        let query = format!(
            "SELECT items.* FROM items JOIN item_tags ON items.id = item_tags.item_id WHERE item_tags.tag_name IN ({}) {} GROUP BY items.id HAVING COUNT(DISTINCT item_tags.tag_name) = {}",
            tags.iter().map(|t| format!("'{}'", t)).collect::<Vec<_>>().join(", "),
            favorite_clause,
            tags.len()
        );

        let items = sqlx::query_as::<_, ResearchItem>(&query)
            .fetch_all(&self.pool)
            .await?;
        Ok(items)
    }

    pub async fn get_item_tags(&self, item_id: i64) -> Result<Vec<Tags>, sqlx::Error> {
        sqlx::query_as::<_, Tags>("SELECT tag_name FROM item_tags WHERE item_id = ?")
            .bind(item_id)
            .fetch_all(&self.pool)
            .await
    }

    pub async fn get_all_items_by_provider(
        &self,
        provider_id: i64,
    ) -> Result<Vec<ResearchItem>, sqlx::Error> {
        sqlx::query_as::<_, ResearchItem>(
            "SELECT * FROM items WHERE provider_id = ? ORDER BY time_added DESC",
        )
        .bind(provider_id)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_all_item_tags(
        &self,
    ) -> Result<Vec<(Vec<Tags>, ResearchItem)>, sqlx::Error> {
        let items = self.get_all_items(None).await?;
        let mut item_tags = Vec::<(Vec<Tags>, ResearchItem)>::new();
        for item in items.iter().filter(|i| i.id.is_some()) {
            let tags = self.get_item_tags(item.id.unwrap()).await?;
            item_tags.push((tags, item.clone()));
        }
        Ok(item_tags)
    }

    pub async fn get_all_tags(&self) -> Result<Vec<Tags>, sqlx::Error> {
        sqlx::query_as::<_, Tags>("SELECT tag_name FROM tags")
            .fetch_all(&self.pool)
            .await
    }

    pub async fn get_secrets(&self) -> Result<Secrets, sqlx::Error> {
        let row = sqlx::query_as::<_, Secrets>("SELECT * FROM secrets")
            .fetch_optional(&self.pool)
            .await?;
        Ok(Secrets::new(row))
    }

    pub async fn set_secret(&self, secrets: Secrets) -> Result<(), sqlx::Error> {
        let _ = sqlx::query(
            "UPDATE secrets SET pocket_consumer_key = ?, pocket_access_token = ? WHERE user_id = (?)",
        )
        .bind(secrets.pocket_consumer_key)
        .bind(secrets.pocket_access_token)
        .bind(DEFAULT_USER_ID)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Comma delimited
    /// Columns: url, folder, title, note, tags, created
    /// url column is required, others are optional
    /// use / to specify nested folder, like a/b/c
    /// to have multiple tags just put them in quotes, like "tag1, tag2"
    /// Export to a csv at the provided file path, or to stdout if no path is provided
    pub async fn export_to_csv(&self, file_path: Option<&str>) -> Result<(), ExportError> {
        let mut builder = WriterBuilder::new();
        let wtr: Box<dyn io::Write> = match file_path {
            Some(path) => Box::new(File::create(path)?),
            None => Box::new(io::stdout()),
        };
        let mut wtr = builder.has_headers(true).from_writer(wtr);

        wtr.write_record(["id", "folder", "url", "title", "note", "tags", "created"])?;

        let items = self.get_all_items(None).await?;
        for item in items {
            let tags = self.get_item_tags(item.id.expect("No ID fetched")).await?;
            let tags = tags
                .iter()
                .map(|t| t.tag_name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            wtr.write_record([
                &item.id.unwrap().to_string(),
                "Research",
                &item.uri,
                &item.title,
                &item.excerpt,
                &tags,
                &item.time_added.to_string(),
            ])?;
        }

        wtr.flush()?;
        Ok(())
    }

    pub async fn mark_as_favorite(&self, item_id: i64, mark: bool) -> Result<(), sqlx::Error> {
        let _ = sqlx::query("UPDATE items SET favorite = ? WHERE id = ?")
            .bind(mark)
            .bind(item_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_item_id(&self, uri: &str) -> Result<Option<i64>, sqlx::Error> {
        let row = sqlx::query("SELECT id FROM items WHERE uri = ?")
            .bind(uri)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get(0)))
    }
}

#[derive(Debug)]
pub enum ExportError {
    Sqlx(sqlx::Error),
    Csv(csv::Error),
    Io(std::io::Error),
}

impl From<sqlx::Error> for ExportError {
    fn from(e: sqlx::Error) -> Self {
        ExportError::Sqlx(e)
    }
}

impl From<csv::Error> for ExportError {
    fn from(e: csv::Error) -> Self {
        ExportError::Csv(e)
    }
}

impl From<std::io::Error> for ExportError {
    fn from(e: std::io::Error) -> Self {
        ExportError::Io(e)
    }
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match *self {
            ExportError::Sqlx(ref e) => e.fmt(f),
            ExportError::Csv(ref e) => e.fmt(f),
            ExportError::Io(ref e) => e.fmt(f),
        }
    }
}

impl Error for ExportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match *self {
            ExportError::Sqlx(ref e) => Some(e),
            ExportError::Csv(ref e) => Some(e),
            ExportError::Io(ref e) => Some(e),
        }
    }
}
