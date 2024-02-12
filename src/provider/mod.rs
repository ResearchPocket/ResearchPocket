use crate::db::{ResearchItem, Secrets, Tags};

pub use pocket::ProviderPocket;
pub mod pocket;

pub trait Insertable {
    fn to_research_item(&self) -> ResearchItem;
    fn to_tags(&self) -> Vec<Tags>;
}

pub trait Provider {
    type Item;
    async fn authenticate(&self) -> Result<Secrets, Box<dyn std::error::Error>>;
    async fn fetch_items(&self) -> Result<Vec<Self::Item>, Box<dyn std::error::Error>>;
}
