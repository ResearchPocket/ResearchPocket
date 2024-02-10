use crate::db::{ResearchItem, Tags};
use sailfish::TemplateOnce;

mod components;

pub struct Site {
    pub html: String,
}

#[derive(TemplateOnce)]
#[template(path = "index.stpl")]
struct IndexTemplate<'a> {
    title: String,
    tags: &'a [Tags],
    item_tags: &'a [(Vec<Tags>, ResearchItem)]
}

const TITLE: &str = "Pocket Research";

impl Site {
    pub fn build(
        tags: &[Tags],
        item_tags: &[(Vec<Tags>, ResearchItem)],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let ctx = IndexTemplate {
            title: TITLE.into(),
            tags,
            item_tags,
        };
        let html = ctx.render_once()?;
        Ok(Self { html })
    }
}
