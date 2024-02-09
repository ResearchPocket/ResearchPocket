use axohtml::{html, text};

use crate::db::Tags;

pub fn tag(tag_name: &str) -> Box<axohtml::elements::li<String>> {
    html! {
        <li style="cursor: pointer; background-color: #f5f5f5; padding: 5px">
            { text!(tag_name) }
        </li>
    }
}

pub fn tags(tags: &[Tags]) -> Box<axohtml::elements::ul<String>> {
    html! {
        <ul style="list-style-type: none; display: inline-flex; flex-wrap: wrap; gap: 10px">
            { tags.iter().map(|Tags {tag_name}| {
                tag(tag_name)
            })}
        </ul>
    }
}
