pub mod pocket;

use self::pocket::PocketItem;
use crate::db::{ResearchItem, Tags};

pub trait Insertable {
    fn to_research_item(&self) -> ResearchItem;
    fn to_tags(&self) -> Vec<Tags>;
}

pub trait Provider {
    type Item: Insertable;

    async fn authenticate(&self) -> Result<(), Box<dyn std::error::Error>>;
    async fn fetch_items(&self) -> Result<Vec<Self::Item>, Box<dyn std::error::Error>>;
}

#[derive(Debug, Default)]
pub struct ProviderPocket {
    pub consumer_key: String,
    pub access_token: Option<String>,
    pub client: reqwest::Client,
}

impl Provider for ProviderPocket {
    type Item = PocketItem;

    async fn authenticate(&self) -> Result<(), Box<dyn std::error::Error>> {
        pocket::login(&self.client, &self.consumer_key).await?;
        Ok(())
    }

    async fn fetch_items(&self) -> Result<Vec<Self::Item>, Box<dyn std::error::Error>> {
        let access_token = self.access_token.as_ref().ok_or("Access token not found")?;
        pocket::get(access_token, &self.consumer_key, &self.client)
            .await
            .map(|items| items.to_vec())
    }
}

impl Insertable for PocketItem {
    fn to_research_item(&self) -> ResearchItem {
        let title = if !self.given_title.is_empty() {
            Some(self.given_title.clone())
        } else if !self.resolved_title.clone().unwrap_or_default().is_empty() {
            self.resolved_title.clone()
        } else {
            Some("Untitled".to_string())
        }
        .unwrap();

        let uri = self
            .given_url
            .as_ref()
            .or(self.resolved_url.as_ref())
            .map_or("#".into(), |url| url.to_string());

        ResearchItem {
            id: self.item_id as i64,
            uri,
            title,
            excerpt: self.excerpt.as_ref().map_or("".to_string(), |s| s.clone()),
            time_added: self.time_added.timestamp(),
            favorite: self.favorite,
            lang: self.lang.clone(),
        }
    }

    fn to_tags(&self) -> Vec<Tags> {
        self.tags.as_ref().map_or(vec![], |tags| {
            tags.iter()
                .map(|tag| Tags {
                    tag_name: tag.tag.clone(),
                })
                .collect()
        })
    }
}
