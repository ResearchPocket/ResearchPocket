//! The document side of the terminal interface.
//!
//! Everything here is a pure projection of one body: Markdown becomes styled
//! rows, `research:item/<uuid>` mentions resolve against targets the caller
//! looked up, and a task line toggles by rewriting one character. Nothing is
//! stored, so the derivation lives and dies with the open document, which is
//! what ADR 0011 and ADR 0012 require of a mention reader.

use std::collections::BTreeMap;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use research_domain::ZenDocumentView;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use url::Url;

use super::terminal_safe;

/// The versioned mention form. New `research:` kinds need an ADR.
pub(super) const MENTION_PREFIX: &str = "research:item/";

/// What one mention resolved to in the local projection, at view time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum MentionTarget {
    Active { title: Option<String>, url: String },
    Deleted { title: Option<String> },
    Unresolved,
}

pub(super) type Mentions = BTreeMap<String, MentionTarget>;

/// One rendered row and the body line it came from.
///
/// Wrapping means a line owns several rows, so the source index is carried
/// rather than recomputed: it is what lets the cursor, the highlight, and a
/// todo toggle all speak about the same line.
pub(super) struct Row {
    pub source: usize,
    pub line: Line<'static>,
}

/// One open document: its body, the mentions it resolved, and where the reader
/// is looking.
pub(super) struct Reader {
    pub document_id: String,
    pub title: Option<String>,
    pub tags: Vec<String>,
    pub deleted: bool,
    pub cursor: usize,
    pub scroll: usize,
    body: String,
    mentions: Mentions,
    revision: u64,
    rows: Vec<Row>,
    rendered: Option<(u16, u64)>,
}

impl Reader {
    pub fn new(view: ZenDocumentView, mentions: Mentions) -> Self {
        Self {
            document_id: view.document_id,
            title: view.title.value,
            tags: view.tags,
            deleted: view.lifecycle.state == research_domain::LifecycleState::Deleted,
            cursor: 0,
            scroll: 0,
            body: view.body,
            mentions,
            revision: 0,
            rows: Vec::new(),
            rendered: None,
        }
    }

    #[cfg(test)]
    fn from_body(body: &str) -> Self {
        Self {
            document_id: "document".to_owned(),
            title: None,
            tags: Vec::new(),
            deleted: false,
            cursor: 0,
            scroll: 0,
            body: body.to_owned(),
            mentions: Mentions::new(),
            revision: 0,
            rows: Vec::new(),
            rendered: None,
        }
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn set_body(&mut self, body: String) {
        self.body = body;
        self.revision += 1;
        self.rendered = None;
        self.cursor = self.cursor.min(self.line_count().saturating_sub(1));
    }

    pub fn set_mentions(&mut self, mentions: Mentions) {
        self.mentions = mentions;
        self.rendered = None;
    }

    /// Body lines addressed the way a toggle rewrites them.
    ///
    /// `split` rather than `lines` so a trailing newline survives the round
    /// trip and an edit never silently trims the end of someone's document.
    pub fn lines(&self) -> std::str::Split<'_, char> {
        self.body.split('\n')
    }

    pub fn line_count(&self) -> usize {
        self.body.split('\n').count()
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let last = self.line_count().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    pub fn cursor_to_last(&mut self) {
        self.cursor = self.line_count().saturating_sub(1);
    }

    /// The body with the cursor line's checkbox flipped, or `None` when that
    /// line is not a task.
    pub fn toggled_body(&self) -> Option<String> {
        let mut lines = self.lines().map(str::to_owned).collect::<Vec<_>>();
        let line = lines.get(self.cursor)?;
        let toggled = toggled_todo_line(line)?;
        lines[self.cursor] = toggled;
        Some(lines.join("\n"))
    }

    /// Rows for this width, rebuilt only when the body or the width changed.
    pub fn rows(&mut self, width: u16) -> &[Row] {
        if self.rendered != Some((width, self.revision)) {
            self.rows = document_rows(&self.body, &self.mentions, usize::from(width));
            self.rendered = Some((width, self.revision));
        }
        &self.rows
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

/// Every item UUID mentioned by a body, in first-seen order and deduplicated.
pub(super) fn mention_ids(body: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut rest = body;
    while let Some(position) = rest.find(MENTION_PREFIX) {
        rest = &rest[position + MENTION_PREFIX.len()..];
        let id = rest
            .chars()
            .take_while(|character| character.is_ascii_hexdigit() || *character == '-')
            .collect::<String>();
        if id.len() == 36 && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// Flips one GFM task marker, keeping the rest of the line byte-identical so
/// the store sees a one-character splice rather than a rewritten line.
pub(super) fn toggled_todo_line(line: &str) -> Option<String> {
    let indent = line.len() - line.trim_start().len();
    let marker = list_marker_len(&line[indent..])?;
    let after = &line[indent + marker..];
    let padding = after.len() - after.trim_start().len();
    let box_start = indent + marker + padding;
    let checkbox = &line.as_bytes()[box_start..];
    if checkbox.len() < 3 || checkbox[0] != b'[' || checkbox[2] != b']' {
        return None;
    }
    let replacement = match checkbox[1] {
        b' ' => "x",
        b'x' | b'X' => " ",
        _ => return None,
    };
    let mut toggled = line.to_owned();
    toggled.replace_range(box_start + 1..box_start + 2, replacement);
    Some(toggled)
}

/// Bytes consumed by a bulleted or ordered list marker, trailing space included.
fn list_marker_len(rest: &str) -> Option<usize> {
    for bullet in ["- ", "* ", "+ "] {
        if rest.starts_with(bullet) {
            return Some(bullet.len());
        }
    }
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    for delimiter in [". ", ") "] {
        if rest[digits..].starts_with(delimiter) {
            return Some(digits + delimiter.len());
        }
    }
    None
}

/// Projects a body into styled, wrapped rows.
///
/// Block structure is read line by line and inline structure only far enough to
/// resolve mentions, links, and code spans. A terminal cannot render Markdown
/// faithfully, and ADR 0011 asks rendering to stay forgiving: anything this
/// does not recognize is shown as the author wrote it.
pub(super) fn document_rows(body: &str, mentions: &Mentions, width: usize) -> Vec<Row> {
    let width = width.max(8);
    let mut rows = Vec::new();
    let mut fenced = false;
    for (source, raw) in body.split('\n').enumerate() {
        let line = terminal_safe(raw.strip_suffix('\r').unwrap_or(raw));
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            wrap_into(&mut rows, source, vec![(line, style_meta())], "", width);
            continue;
        }
        if fenced {
            wrap_into(&mut rows, source, vec![(line, style_code())], "  ", width);
            continue;
        }
        if trimmed.is_empty() {
            rows.push(Row {
                source,
                line: Line::default(),
            });
            continue;
        }
        if is_thematic_break(trimmed) {
            rows.push(Row {
                source,
                line: Line::styled("─".repeat(width), style_meta()),
            });
            continue;
        }
        let (segments, indent) = block_segments(&line, mentions);
        wrap_into(&mut rows, source, segments, &indent, width);
    }
    rows
}

fn is_thematic_break(trimmed: &str) -> bool {
    ["-", "*", "_"].into_iter().any(|mark| {
        trimmed.len() >= 3
            && trimmed
                .chars()
                .all(|character| character.to_string() == mark)
    })
}

/// Splits one block-level line into styled segments plus the indent that its
/// wrapped continuation rows should hang under.
fn block_segments(line: &str, mentions: &Mentions) -> (Vec<(String, Style)>, String) {
    let indent = &line[..line.len() - line.trim_start().len()];
    let rest = line.trim_start();

    let hashes = rest.len() - rest.trim_start_matches('#').len();
    if (1..=6).contains(&hashes) && rest[hashes..].starts_with(' ') {
        let style = if hashes <= 2 {
            style_heading().add_modifier(Modifier::BOLD)
        } else {
            style_heading()
        };
        return (
            vec![(rest[hashes + 1..].to_owned(), style)],
            indent.to_owned(),
        );
    }

    if let Some(quoted) = rest.strip_prefix('>') {
        let mut segments = vec![("│ ".to_owned(), style_meta())];
        segments.extend(inline_segments(
            quoted.trim_start(),
            mentions,
            style_quote(),
        ));
        return (segments, format!("{indent}│ "));
    }

    if let Some(marker) = list_marker_len(rest) {
        let after = &rest[marker..];
        let padding = after.len() - after.trim_start().len();
        let content = after.trim_start();
        let checkbox = content.as_bytes();
        let task = (checkbox.len() >= 3 && checkbox[0] == b'[' && checkbox[2] == b']')
            .then(|| match checkbox[1] {
                b' ' => Some(("[ ] ", style_todo_open())),
                b'x' | b'X' => Some(("[x] ", style_todo_done())),
                _ => None,
            })
            .flatten();
        if let Some((mark, style)) = task {
            let mut segments = vec![
                (indent.to_owned(), Style::default()),
                (mark.to_owned(), style),
            ];
            segments.extend(inline_segments(
                content[3..].trim_start(),
                mentions,
                Style::default(),
            ));
            return (segments, format!("{indent}    "));
        }
        let bullet = if rest.starts_with(['-', '*', '+']) {
            "• ".to_owned()
        } else {
            rest[..marker].to_owned()
        };
        let hanging = " ".repeat(UnicodeWidthStr::width(bullet.as_str()) + padding);
        let mut segments = vec![
            (indent.to_owned(), Style::default()),
            (bullet, style_heading()),
        ];
        segments.extend(inline_segments(content, mentions, Style::default()));
        return (segments, format!("{indent}{hanging}"));
    }

    (
        inline_segments(line, mentions, Style::default()),
        indent.to_owned(),
    )
}

/// Reads links, mentions, and code spans out of one line of prose.
fn inline_segments(text: &str, mentions: &Mentions, base: Style) -> Vec<(String, Style)> {
    let mut segments: Vec<(String, Style)> = Vec::new();
    let mut plain = String::new();
    let characters = text.char_indices().collect::<Vec<_>>();
    let mut index = 0;
    while index < characters.len() {
        let (offset, character) = characters[index];
        if character == '`'
            && let Some(end) = text[offset + 1..].find('`')
        {
            flush(&mut segments, &mut plain, base);
            segments.push((text[offset + 1..offset + 1 + end].to_owned(), style_code()));
            index += text[offset..offset + 2 + end].chars().count();
            continue;
        }
        if character == '['
            && let Some(link) = parse_link(&text[offset..])
        {
            flush(&mut segments, &mut plain, base);
            let image = segments
                .last()
                .and_then(|(value, _)| value.chars().last())
                .is_some_and(|last| last == '!');
            if image && let Some((value, _)) = segments.last_mut() {
                value.pop();
            }
            segments.extend(link_segments(&link.label, &link.destination, mentions));
            index += text[offset..offset + link.length].chars().count();
            continue;
        }
        plain.push(character);
        index += 1;
    }
    flush(&mut segments, &mut plain, base);
    segments
}

fn flush(segments: &mut Vec<(String, Style)>, plain: &mut String, base: Style) {
    if !plain.is_empty() {
        segments.push((std::mem::take(plain), base));
    }
}

struct InlineLink {
    label: String,
    destination: String,
    length: usize,
}

/// Parses `[label](destination)` starting at `text`, without nesting.
fn parse_link(text: &str) -> Option<InlineLink> {
    let label_end = text.find("](")?;
    let rest = &text[label_end + 2..];
    let destination_end = rest.find(')')?;
    let destination = rest[..destination_end].trim();
    if destination.is_empty() || text[1..label_end].contains('[') {
        return None;
    }
    Some(InlineLink {
        label: text[1..label_end].to_owned(),
        destination: destination.to_owned(),
        length: label_end + 2 + destination_end + 1,
    })
}

/// Renders one link, resolving mentions against the projection the caller read.
fn link_segments(label: &str, destination: &str, mentions: &Mentions) -> Vec<(String, Style)> {
    let Some(item_id) = destination.strip_prefix(MENTION_PREFIX) else {
        let mut segments = vec![(label.to_owned(), style_link())];
        if label != destination {
            segments.push((format!(" ({destination})"), style_meta()));
        }
        return segments;
    };
    match mentions.get(item_id) {
        Some(MentionTarget::Active { title, url }) => vec![
            (
                title
                    .clone()
                    .filter(|title| !title.is_empty())
                    .unwrap_or_else(|| label.to_owned()),
                style_link(),
            ),
            (format!(" · {}", host(url)), style_meta()),
        ],
        Some(MentionTarget::Deleted { title }) => vec![
            (
                title
                    .clone()
                    .filter(|title| !title.is_empty())
                    .unwrap_or_else(|| label.to_owned()),
                style_quote().add_modifier(Modifier::CROSSED_OUT),
            ),
            (" · deleted save".to_owned(), style_meta()),
        ],
        Some(MentionTarget::Unresolved) | None => vec![
            (format!("{MENTION_PREFIX}{item_id}"), style_unresolved()),
            (" · unresolved".to_owned(), style_meta()),
        ],
    }
}

fn host(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .unwrap_or_else(|| "saved link".to_owned())
}

/// Greedy word wrapping that keeps styles intact across a break.
fn wrap_into(
    rows: &mut Vec<Row>,
    source: usize,
    segments: Vec<(String, Style)>,
    indent: &str,
    width: usize,
) {
    let indent = if UnicodeWidthStr::width(indent) + 8 <= width {
        indent
    } else {
        ""
    };
    let mut wrapper = Wrapper::new(rows, source, indent, width);
    for (text, style) in segments {
        for token in tokenize(&text) {
            wrapper.push(token, style);
        }
    }
    wrapper.finish();
}

struct Wrapper<'a> {
    rows: &'a mut Vec<Row>,
    source: usize,
    indent: String,
    indent_width: usize,
    width: usize,
    spans: Vec<Span<'static>>,
    used: usize,
    filled: bool,
}

impl<'a> Wrapper<'a> {
    fn new(rows: &'a mut Vec<Row>, source: usize, indent: &str, width: usize) -> Self {
        Self {
            rows,
            source,
            indent: indent.to_owned(),
            indent_width: UnicodeWidthStr::width(indent),
            width,
            spans: Vec::new(),
            used: 0,
            filled: false,
        }
    }

    fn push(&mut self, token: String, style: Style) {
        if token.chars().all(char::is_whitespace) {
            if !self.filled {
                return;
            }
            if self.used + UnicodeWidthStr::width(token.as_str()) > self.width {
                self.wrap();
            } else {
                self.write(token, style);
            }
            return;
        }
        let mut word = token;
        loop {
            if self.used + UnicodeWidthStr::width(word.as_str()) <= self.width {
                self.write(word, style);
                return;
            }
            if self.filled {
                self.wrap();
                continue;
            }
            // A single word wider than the row: break it rather than loop.
            let head = split_at_width(&word, self.width.saturating_sub(self.used));
            if head.is_empty() {
                self.write(word, style);
                return;
            }
            word = word[head.len()..].to_owned();
            self.write(head, style);
            self.wrap();
        }
    }

    fn write(&mut self, text: String, style: Style) {
        self.used += UnicodeWidthStr::width(text.as_str());
        self.filled = true;
        self.spans.push(Span::styled(text, style));
    }

    /// Ends the current row and hangs the next one under the block's indent.
    fn wrap(&mut self) {
        self.rows.push(Row {
            source: self.source,
            line: Line::from(std::mem::take(&mut self.spans)),
        });
        self.used = self.indent_width;
        self.filled = false;
        if !self.indent.is_empty() {
            self.spans.push(Span::raw(self.indent.clone()));
        }
    }

    fn finish(self) {
        self.rows.push(Row {
            source: self.source,
            line: Line::from(self.spans),
        });
    }
}

/// Splits a line into runs of whitespace and runs of visible text.
fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut whitespace = None;
    for character in text.chars() {
        let is_whitespace = character.is_whitespace();
        if whitespace != Some(is_whitespace) && !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
        whitespace = Some(is_whitespace);
        current.push(character);
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// The longest prefix of `word` that fits in `available` columns.
fn split_at_width(word: &str, available: usize) -> String {
    let mut head = String::new();
    let mut used = 0;
    for character in word.chars() {
        let next = used + UnicodeWidthChar::width(character).unwrap_or(0);
        if next > available {
            break;
        }
        used = next;
        head.push(character);
    }
    head
}

pub(super) fn format_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    }
}

/// Unix seconds as a local calendar day, or the raw value if it cannot be read.
pub(super) fn format_timestamp(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|stamp| stamp.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| seconds.to_string())
}

fn style_heading() -> Style {
    Style::default().fg(Color::Cyan)
}

fn style_meta() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn style_quote() -> Style {
    Style::default().fg(Color::Gray)
}

fn style_code() -> Style {
    Style::default().fg(Color::Yellow)
}

fn style_link() -> Style {
    Style::default()
        .fg(Color::Blue)
        .add_modifier(Modifier::UNDERLINED)
}

fn style_todo_done() -> Style {
    Style::default().fg(Color::Green)
}

fn style_todo_open() -> Style {
    Style::default().fg(Color::Magenta)
}

fn style_unresolved() -> Style {
    Style::default().fg(Color::Red)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                row.line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn mentions_resolve_against_the_projection_at_view_time() {
        let mut mentions = Mentions::new();
        mentions.insert(
            "a".repeat(36),
            MentionTarget::Active {
                title: Some("Loro CRDT".to_owned()),
                url: "https://loro.dev/docs".to_owned(),
            },
        );
        mentions.insert(
            "b".repeat(36),
            MentionTarget::Deleted {
                title: Some("Removed save".to_owned()),
            },
        );
        let body = format!(
            "See [old label](research:item/{}), [gone](research:item/{}), and \
             [missing](research:item/{}).",
            "a".repeat(36),
            "b".repeat(36),
            "c".repeat(36)
        );

        let rendered = text(&document_rows(&body, &mentions, 200)).join("\n");
        assert!(
            rendered.contains("Loro CRDT · loro.dev"),
            "an active mention renders the item's current title and host: {rendered}"
        );
        assert!(rendered.contains("Removed save · deleted save"));
        assert!(rendered.contains(&format!("{MENTION_PREFIX}{} · unresolved", "c".repeat(36))));
        assert!(
            !rendered.contains("old label"),
            "the stored label never wins over live item state"
        );
    }

    #[test]
    fn blocks_render_and_wrap_while_keeping_their_source_line() {
        let body = "# Heading\n\n- [ ] open task\n- [x] done task\n\n> quoted\n\n```\ncode  spans\n```\n\nplain";
        let rows = document_rows(body, &Mentions::new(), 40);
        let rendered = text(&rows);
        assert_eq!(rendered[0], "Heading");
        assert_eq!(rendered[2], "[ ] open task");
        assert_eq!(rendered[3], "[x] done task");
        assert_eq!(rendered[5], "│ quoted");
        assert_eq!(rendered[8], "code  spans", "code keeps its own spacing");
        assert_eq!(rendered[11], "plain");
        assert_eq!(rows.len(), body.split('\n').count());

        let long = format!("- {}", "word ".repeat(20));
        let wrapped = document_rows(&long, &Mentions::new(), 20);
        assert!(wrapped.len() > 1);
        assert!(
            wrapped.iter().all(|row| row.source == 0),
            "every wrapped row still points at the line it came from"
        );
        assert!(
            wrapped[1]
                .line
                .spans
                .first()
                .is_some_and(|span| span.content.starts_with("  ")),
            "continuations hang under the list marker"
        );
        for row in &wrapped {
            let width: usize = row
                .line
                .spans
                .iter()
                .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                .sum();
            assert!(width <= 20, "row overflows its column: {width}");
        }
    }

    #[test]
    fn an_unbreakable_word_is_split_rather_than_looping() {
        let rows = document_rows(&"x".repeat(50), &Mentions::new(), 12);
        assert_eq!(rows.len(), 5);
        assert!(text(&rows).iter().all(|row| row.len() <= 12));
    }

    #[test]
    fn toggling_rewrites_exactly_one_character() {
        assert_eq!(
            toggled_todo_line("  - [ ] ship it").as_deref(),
            Some("  - [x] ship it")
        );
        assert_eq!(
            toggled_todo_line("3) [X] ordered").as_deref(),
            Some("3) [ ] ordered")
        );
        assert_eq!(toggled_todo_line("- plain bullet"), None);
        assert_eq!(toggled_todo_line("not a list [ ] at all"), None);

        let original = "- [ ] ship it";
        let toggled = toggled_todo_line(original).expect("task line");
        assert_eq!(
            original
                .bytes()
                .zip(toggled.bytes())
                .filter(|(before, after)| before != after)
                .count(),
            1
        );
    }

    #[test]
    fn mention_ids_are_deduplicated_and_must_be_whole_uuids() {
        let full = "0197f2b5-93d7-7ad4-8c67-21e98f0c7341";
        let body = format!(
            "[a](research:item/{full}) [b](research:item/{full}) [c](research:item/nope)"
        );
        assert_eq!(mention_ids(&body), [full.to_owned()]);
    }

    #[test]
    fn a_trailing_newline_survives_a_toggle() {
        let mut reader = Reader::from_body("- [ ] one\n- [ ] two\n");
        reader.cursor = 1;
        assert_eq!(
            reader.toggled_body().as_deref(),
            Some("- [ ] one\n- [x] two\n"),
            "the empty line after a trailing newline is preserved"
        );
        assert_eq!(reader.line_count(), 3);

        reader.cursor = 2;
        assert_eq!(reader.toggled_body(), None);
    }

    #[test]
    fn the_cursor_stays_inside_the_body() {
        let mut reader = Reader::from_body("one\ntwo\nthree");
        reader.move_cursor(10);
        assert_eq!(reader.cursor, 2);
        reader.move_cursor(-10);
        assert_eq!(reader.cursor, 0);
        reader.cursor_to_last();
        assert_eq!(reader.cursor, 2);
        reader.set_body("one".to_owned());
        assert_eq!(reader.cursor, 0, "a shorter body pulls the cursor back");
    }
}
