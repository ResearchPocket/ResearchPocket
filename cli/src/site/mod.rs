use crate::db::{ResearchItem, Tags};
use sailfish::TemplateOnce;
use serde::Serialize;

pub struct Site {
    pub index_html: String,
    pub search_html: String,
}

#[derive(TemplateOnce, Serialize)]
#[template(path = "index.stpl")]
#[template(rm_whitespace = true)]
struct IndexTemplate<'a> {
    title: &'a str,
    css_path: &'a str,
    tags: Vec<&'a str>,
    item_tags: &'a [(Vec<Tags>, ResearchItem)],
}

#[derive(TemplateOnce, Serialize)]
#[template(path = "search.stpl")]
#[template(rm_whitespace = true)]
struct SearchTemplate<'a> {
    title: &'a str,
    css_path: &'a str,
    item_tags: Vec<ItemTag<'a>>,
}

#[derive(Serialize)]
struct ItemTag<'a> {
    pub tags: Vec<&'a str>,
    #[serde(flatten)]
    pub item: &'a ResearchItem,
}

const TITLE: &str = "Pocket Research";

impl Site {
    pub fn build(
        tags: &[Tags],
        item_tags: &[(Vec<Tags>, ResearchItem)],
        css_file: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let ctx = IndexTemplate {
            title: TITLE,
            tags: tags.iter().map(|t| t.tag_name.as_str()).collect::<Vec<_>>(),
            item_tags,
            css_path: css_file,
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
            css_path: css_file,
            title: "Search",
        };
        let search_html = ctx.render_once()?;
        Ok(Self {
            index_html,
            search_html,
        })
    }
}
