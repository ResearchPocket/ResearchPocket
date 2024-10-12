use crate::db::{ResearchItem, Secrets, Tags};

pub use pocket::ProviderPocket;
pub mod local;
pub mod pocket;

pub trait Insertable {
    fn to_research_item(&self) -> ResearchItem;
    fn to_tags(&self) -> Vec<Tags>;
}

pub trait Provider {
    type Item;
}

pub trait OnlineProvider: Provider {
    async fn authenticate(&self) -> Result<Secrets, Box<dyn std::error::Error>>;
    async fn fetch_items(
        &self,
        limit: Option<usize>,
    ) -> Result<Vec<Self::Item>, Box<dyn std::error::Error>>;
    async fn add_item(
        &self,
        uri: &str,
        tags: Vec<&str>,
    ) -> Result<Option<i64>, Box<dyn std::error::Error>>;
    async fn mark_as_favorite(
        &self,
        item_id: i64,
        mark: bool,
    ) -> Result<(), Box<dyn std::error::Error>>;
}
