use crate::db::{ResearchItem, Tags};
use chrono_tz::Tz;
use sailfish::TemplateOnce;
use serde::Serialize;
use std::sync::RwLock;

pub struct Site {
    pub index_html: String,
    pub search_html: String,
}

#[derive(TemplateOnce, Serialize)]
#[template(path = "index.stpl")]
#[template(rm_whitespace = true)]
struct IndexTemplate<'a> {
    title: &'a str,
    assets_dir: &'a str,
    tags: Vec<&'a str>,
    item_tags: &'a [(Vec<Tags>, ResearchItem)],
}

#[derive(TemplateOnce, Serialize)]
#[template(path = "search.stpl")]
#[template(rm_whitespace = true)]
struct SearchTemplate<'a> {
    title: &'a str,
    assets_dir: &'a str,
    item_tags: Vec<ItemTag<'a>>,
    tags: Vec<&'a str>,
}

#[derive(Serialize)]
struct ItemTag<'a> {
    pub tags: Vec<&'a str>,
    #[serde(flatten)]
    pub item: &'a ResearchItem,
}

static TIMEZONE: RwLock<Option<Tz>> = RwLock::new(None);

const TITLE: &str = "Pocket Research";

impl Site {
    pub fn build(
        tags: &[Tags],
        item_tags: &[(Vec<Tags>, ResearchItem)],
        assets_dir: &str,
        timezone: Option<Tz>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        {
            let mut timezone_lock = TIMEZONE.write().unwrap();
            *timezone_lock = timezone;
        }
        let tags = tags.iter().map(|t| t.tag_name.as_str()).collect::<Vec<_>>();
        let ctx = IndexTemplate {
            title: TITLE,
            item_tags,
            assets_dir,
            tags: tags.clone(),
        };

        let index_html = ctx.render_once()?;

        let item_tags = item_tags
            .iter()
            .map(|(tags, item)| ItemTag {
                tags: tags.iter().map(|t| t.tag_name.as_str()).collect(),
                item,
            })
            .collect::<Vec<_>>();

        let ctx = SearchTemplate {
            item_tags,
            assets_dir,
            title: "Search",
            tags: tags.clone(),
        };
        let search_html = ctx.render_once()?;
        Ok(Self {
            index_html,
            search_html,
        })
    }
}
