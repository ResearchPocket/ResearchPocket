use super::{Insertable, OnlineProvider, Provider, ResearchItem};
use crate::db::{Secrets, Tags};
use api::{add, favorite, get, login, PocketItem};
use chrono::Utc;

pub mod api;

#[derive(Debug, Default)]
pub struct ProviderPocket {
    pub consumer_key: String,
    pub access_token: Option<String>,
    pub client: reqwest::Client,
}

impl Provider for ProviderPocket {
    type Item = PocketItem;
}

impl OnlineProvider for ProviderPocket {
    async fn authenticate(&self) -> Result<Secrets, Box<dyn std::error::Error>> {
        let access_token = login(&self.client, &self.consumer_key).await?;
        Ok(Secrets {
            pocket_consumer_key: Some(self.consumer_key.clone()),
            pocket_access_token: Some(access_token),
            ..Default::default()
        })
    }

    async fn fetch_items(
        &self,
        limit: Option<usize>,
    ) -> Result<Vec<PocketItem>, Box<dyn std::error::Error>> {
        let access_token = self.access_token.as_ref().ok_or("Access token not found")?;
        get(access_token, &self.consumer_key, &self.client, limit).await
    }

    async fn add_item(
        &self,
        uri: &str,
        tags: Vec<&str>,
    ) -> Result<Option<i64>, Box<dyn std::error::Error>> {
        let access_token = self.access_token.as_ref().ok_or("Access token not found")?;
        let add_request = api::PocketAddRequest {
            url: uri,
            title: None,
            tags: Some(&tags),
        };
        let item_id = add(&self.client, access_token, &self.consumer_key, add_request).await?;
        Ok(Some(item_id))
    }

    async fn mark_as_favorite(
        &self,
        item_id: i64,
        mark: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let access_token = self.access_token.as_ref().ok_or("Access token not found")?;
        favorite(
            &self.client,
            access_token,
            &self.consumer_key,
            item_id,
            mark,
        )
        .await?;
        Ok(())
    }
}

impl Insertable for PocketItem {
    fn to_research_item(&self) -> ResearchItem {
        let title = self
            .given_title
            .as_ref()
            .filter(|title| !title.is_empty())
            .or_else(|| {
                self.resolved_title
                    .as_ref()
                    .filter(|title| !title.is_empty())
            })
            .cloned()
            .unwrap_or_else(|| "Untitled".to_string());

        let uri = self
            .given_url
            .as_ref()
            .or(self.resolved_url.as_ref())
            .map_or("#".into(), |url| url.to_string());

        ResearchItem {
            id: Some(self.item_id as i64),
            uri,
            title,
            excerpt: self.excerpt.as_ref().map_or("".to_string(), |s| s.clone()),
            time_added: self.time_added.unwrap_or(Utc::now()).timestamp(),
            favorite: self.favorite.unwrap_or(false),
            lang: self.lang.clone(),
            notes: None,
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
