use crate::db::{ResearchItem, Tags};
use axohtml::{dom::DOMTree, html, text};

mod components;

pub struct Site {
    pub html: String,
}

const TITLE: &str = "Pocket Research";

impl Site {
    pub fn build(
        tags: &[Tags],
        item_tags: &[(Vec<Tags>, ResearchItem)],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let doc: DOMTree<String> = html!(
            <html lang="en" class="">
                <head>
                    <title>{ text!(TITLE) }</title>
                </head>

                <body>
                    <div class="container">
                        <h1>{ text!(TITLE) }</h1>
                        <h2>"Tags"</h2>
                        {components::tags(tags)}
                        <h2>"Items"</h2>
                        <ul style="list-style-type: none; word-wrap: break-word">
                            { item_tags.iter().map(|(tags, item)| {
                                html! (
                                    <li style="border: 1px solid #f5f5f5; padding: 10px; margin: 10px;">
                                        <a href=&item.uri>{ text!(&item.title) }</a>
                                        <p>{ text!(&item.format_time_added()) }</p>
                                        "Tags:"{components::tags(tags)}
                                        <p>{ text!(&item.excerpt) }</p>
                                    </li>
                                )
                            })}
                        </ul>
                    </div>
                </body>
            </html>
        );

        Ok(Site {
            html: doc.to_string(),
        })
    }
}
