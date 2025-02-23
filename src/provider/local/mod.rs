use crate::db::{ResearchItem, Tags};

use super::{Insertable, Provider};

#[derive(Debug, Default)]
pub struct ProviderLocal;

pub struct LocalItem {
    // shouldn't be needed for local items
    pub id: Option<i64>,
    pub uri: String,
    pub title: Option<String>,
    pub excerpt: Option<String>,
    pub time_added: i64,
    pub tags: Vec<Tags>,
}

impl Provider for ProviderLocal {
    type Item = LocalItem;
}

impl Insertable for LocalItem {
    fn to_research_item(&self) -> crate::db::ResearchItem {
        ResearchItem {
            id: self.id,
            uri: self.uri.clone(),
            title: self.title.clone().unwrap_or("Untitled".to_string()),
            excerpt: self.excerpt.clone().unwrap_or("".to_string()),
            time_added: self.time_added,
            favorite: false,
            lang: Some("en".into()),
            notes: None,
        }
    }

    fn to_tags(&self) -> Vec<crate::db::Tags> {
        self.tags.clone()
    }
}
