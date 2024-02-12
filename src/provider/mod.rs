use crate::db::{ResearchItem, Tags};

pub use pocket::ProviderPocket;
pub mod pocket;

pub trait Insertable {
    fn to_research_item(&self) -> ResearchItem;
    fn to_tags(&self) -> Vec<Tags>;
}

pub trait Provider<T> {
    async fn authenticate(&self) -> Result<(), Box<dyn std::error::Error>>;
    async fn fetch_items(&self) -> Result<Vec<T>, Box<dyn std::error::Error>>;
}

pub struct Providers {
    pub pocket: Option<ProviderPocket>,
}
