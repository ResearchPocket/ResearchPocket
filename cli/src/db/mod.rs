use sqlx::{sqlite::SqlitePoolOptions, FromRow, Pool, Row, Sqlite};

#[derive(Clone, FromRow, Debug)]
#[allow(dead_code)]
pub struct Providers {
    pub id: i64,
    pub name: String,
    pub secret: Option<String>,
}

#[derive(Clone, FromRow, Debug, sqlx::Type)]
pub struct Tags {
    pub tag_name: String,
}

#[derive(Clone, FromRow, Debug)]
pub struct ResearchItem {
    pub id: i64,
    pub uri: String,
    pub title: String,
    pub excerpt: String,
    pub time_added: i64,
    pub favorite: bool,
    pub lang: Option<String>,
}

impl ResearchItem {
    /// Of the format "21 Aug'21, 5pm"
    pub fn format_time_added(&self) -> String {
        let date = chrono::NaiveDateTime::from_timestamp_opt(self.time_added, 0).unwrap();
        date.format("%d %b'%y, %l%P").to_string()
    }
}

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
        tags: Vec<Tags>,
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
            .bind(tag.tag_name)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn get_all_items(&self) -> Result<Vec<ResearchItem>, sqlx::Error> {
        // get all items sorted by time_added
        sqlx::query_as::<_, ResearchItem>("SELECT * FROM items ORDER BY time_added DESC")
            .fetch_all(&self.pool)
            .await
    }

    pub async fn get_item_tags(&self, item_id: i64) -> Result<Vec<Tags>, sqlx::Error> {
        sqlx::query_as::<_, Tags>("SELECT tag_name FROM item_tags WHERE item_id = ?")
            .bind(item_id)
            .fetch_all(&self.pool)
            .await
    }

    pub async fn get_all_item_tags(
        &self,
    ) -> Result<Vec<(Vec<Tags>, ResearchItem)>, sqlx::Error> {
        let items = self.get_all_items().await?;
        let mut item_tags = Vec::<(Vec<Tags>, ResearchItem)>::new();
        for item in items {
            let tags = self.get_item_tags(item.id).await?;
            item_tags.push((tags, item));
        }
        Ok(item_tags)
    }

    pub async fn get_all_tags(&self) -> Result<Vec<Tags>, sqlx::Error> {
        sqlx::query_as::<_, Tags>("SELECT tag_name FROM tags")
            .fetch_all(&self.pool)
            .await
    }
}
