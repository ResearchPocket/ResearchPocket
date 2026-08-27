use std::collections::{BTreeSet, VecDeque};
use std::env;
use std::error::Error;
use std::fs;
use std::io::{self, IsTerminal, Stdout, Write};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, SetCursorStyle, Show};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use research_domain::{LifecycleState, MAX_ZEN_BODY_BYTES, ZenDocumentSummary};
use research_store::{
    CreateItemRequest, CreateZenDocumentRequest, EditItemRequest, EditZenDocumentRequest,
    EnrichmentStatus as StoreEnrichmentStatus, ListQuery, OptionalTextUpdate, SearchQuery,
    StoreError, StoreStatus, StoredItem, V2Store, ZenListQuery,
};
use serde_json::Value;
use tokio::task::JoinHandle;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{sync, v2};

mod zen;

use zen::{MentionTarget, Mentions, Reader, format_bytes, format_timestamp, mention_ids};

type TuiResult<T> = Result<T, Box<dyn Error>>;

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const ACTION_LATCH_IDLE: Duration = Duration::from_secs(2);
const MIN_COMFORTABLE_WIDTH: u16 = 72;
/// Widest prose column the reader will use, gutter included.
const READING_COLUMN: u16 = 88;
const GUTTER_WIDTH: u16 = 2;
const MIN_COMFORTABLE_HEIGHT: u16 = 20;

pub async fn run(store: &V2Store, data_dir: &Path) -> TuiResult<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other("the TUI requires an interactive terminal").into());
    }

    let mut terminal = TerminalSession::new()?;
    let shutdown = install_signal_handlers()?;
    let mut app = App::load(store).await?;

    while !app.should_quit && !shutdown.load(Ordering::Relaxed) {
        if app.operation_finished() {
            app.finish_operation(store, data_dir).await;
        }
        terminal.terminal.draw(|frame| render(frame, &mut app))?;
        if !event::poll(EVENT_POLL_INTERVAL)? {
            app.clear_action_latch();
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press && app.accept_key_press(key) => {
                app.handle_key(store, data_dir, &mut terminal, key).await;
            }
            Event::Key(key) if key.kind == KeyEventKind::Repeat => {
                app.note_key_repeat(key);
                app.handle_repeat_key(key);
            }
            Event::Key(key) if key.kind == KeyEventKind::Release => {
                app.release_key(key);
            }
            Event::Paste(text) => app.handle_paste(text),
            Event::Resize(_, _) => {}
            _ => {}
        }
    }

    Ok(())
}

fn install_signal_handlers() -> io::Result<Arc<AtomicBool>> {
    let shutdown = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    for signal in [
        signal_hook::consts::signal::SIGHUP,
        signal_hook::consts::signal::SIGINT,
        signal_hook::consts::signal::SIGTERM,
    ] {
        signal_hook::flag::register(signal, Arc::clone(&shutdown))?;
    }
    Ok(shutdown)
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    fn new() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            SetCursorStyle::BlinkingBar,
            Hide
        ) {
            let _ = execute!(
                stdout,
                Show,
                SetCursorStyle::DefaultUserShape,
                DisableBracketedPaste,
                LeaveAlternateScreen
            );
            let _ = disable_raw_mode();
            return Err(error);
        }
        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let mut stdout = io::stdout();
                let _ = execute!(
                    stdout,
                    Show,
                    SetCursorStyle::DefaultUserShape,
                    DisableBracketedPaste,
                    LeaveAlternateScreen
                );
                let _ = disable_raw_mode();
                Err(error)
            }
        }
    }

    fn run_child(&mut self, command: &mut Command) -> io::Result<std::process::ExitStatus> {
        if let Err(error) = self.suspend() {
            let _ = self.resume();
            return Err(error);
        }

        let child_result = command.status();
        let resume_result = self.resume();
        match (child_result, resume_result) {
            (_, Err(error)) => Err(error),
            (result, Ok(())) => result,
        }
    }

    fn suspend(&mut self) -> io::Result<()> {
        self.terminal.show_cursor()?;
        execute!(
            self.terminal.backend_mut(),
            Show,
            SetCursorStyle::DefaultUserShape,
            DisableBracketedPaste,
            LeaveAlternateScreen
        )?;
        disable_raw_mode()
    }

    fn resume(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        execute!(
            self.terminal.backend_mut(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            SetCursorStyle::BlinkingBar,
            Hide
        )?;
        // The child editor may have replaced every visible cell. Ratatui still
        // remembers the frame from before suspension, so invalidate its cached
        // previous buffer and make the next draw repaint the whole interface.
        self.terminal.swap_buffers();
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            Show,
            SetCursorStyle::DefaultUserShape,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}

struct App {
    workspace: Workspace,
    items: Vec<StoredItem>,
    selected: usize,
    list_state: ListState,
    query: String,
    favorite_only: bool,
    lifecycle: LifecycleFilter,
    documents: Vec<ZenDocumentSummary>,
    document_selected: usize,
    document_list_state: ListState,
    document_query: String,
    document_lifecycle: LifecycleFilter,
    /// The one open document, body included. Dropped when the reader closes,
    /// so no body and no derived mention map outlives the view.
    reader: Option<Reader>,
    status: StoreStatus,
    /// Aggregate-scoped operations waiting to upload. Document work queues
    /// here rather than in the protocol-v1 outbox `status` counts.
    aggregates_pending: u64,
    mode: Mode,
    notice: Option<Notice>,
    detail_scroll: u16,
    should_quit: bool,
    action_latch: Option<(ModeKind, KeyCode, KeyModifiers, Instant)>,
    operation: Option<BackgroundOperation>,
    queued_enrichment: VecDeque<(String, &'static str)>,
}

impl App {
    async fn load(store: &V2Store) -> TuiResult<Self> {
        let status = store.status().await?;
        let mut app = Self {
            workspace: Workspace::Library,
            items: Vec::new(),
            selected: 0,
            list_state: ListState::default(),
            query: String::new(),
            favorite_only: false,
            lifecycle: LifecycleFilter::Active,
            documents: Vec::new(),
            document_selected: 0,
            document_list_state: ListState::default(),
            document_query: String::new(),
            document_lifecycle: LifecycleFilter::Active,
            reader: None,
            status,
            aggregates_pending: store.pending_aggregate_operation_count().await?,
            mode: Mode::Browse,
            notice: None,
            detail_scroll: 0,
            should_quit: false,
            action_latch: None,
            operation: None,
            queued_enrichment: VecDeque::new(),
        };
        app.refresh(store, None).await?;
        Ok(app)
    }

    async fn refresh(&mut self, store: &V2Store, preferred_id: Option<&str>) -> TuiResult<()> {
        let selected_id = preferred_id
            .map(str::to_owned)
            .or_else(|| self.selected_item().map(|item| item.id.clone()));
        let include_deleted = self.lifecycle != LifecycleFilter::Active;
        let mut items = if self.query.trim().is_empty() {
            store
                .list(ListQuery {
                    favorite_only: self.favorite_only,
                    include_deleted,
                    limit: None,
                    ..ListQuery::default()
                })
                .await?
                .items
        } else {
            store
                .search(SearchQuery {
                    text: self.query.clone(),
                    favorite_only: self.favorite_only,
                    include_deleted,
                    limit: None,
                    ..SearchQuery::default()
                })
                .await?
                .items
        };
        if self.lifecycle == LifecycleFilter::Deleted {
            items.retain(|item| item.state == "deleted");
        }
        let status = store.status().await?;

        let next_selected = selected_id
            .as_ref()
            .and_then(|id| items.iter().position(|item| item.id == *id))
            .unwrap_or_else(|| self.selected.min(items.len().saturating_sub(1)));
        let next_selected_id = items.get(next_selected).map(|item| item.id.as_str());
        if selected_id.as_deref() != next_selected_id {
            self.detail_scroll = 0;
        }
        self.items = items;
        self.selected = next_selected;
        self.sync_selection();
        self.status = status;
        self.aggregates_pending = store.pending_aggregate_operation_count().await?;
        self.refresh_documents(store, None).await
    }

    /// Reloads the workspace index. Metadata only: opening or refreshing the
    /// workspace never reads a body.
    async fn refresh_documents(
        &mut self,
        store: &V2Store,
        preferred_id: Option<&str>,
    ) -> TuiResult<()> {
        let selected_id = preferred_id.map(str::to_owned).or_else(|| {
            self.selected_document()
                .map(|document| document.document_id.clone())
        });
        let mut documents = store
            .list_zen_documents_with(ZenListQuery {
                include_deleted: self.document_lifecycle != LifecycleFilter::Active,
            })
            .await?;
        if self.document_lifecycle == LifecycleFilter::Deleted {
            documents.retain(|document| document.lifecycle_state == LifecycleState::Deleted);
        }
        let needle = self.document_query.trim().to_lowercase();
        if !needle.is_empty() {
            documents.retain(|document| document_matches(document, &needle));
        }
        self.document_selected = selected_id
            .as_ref()
            .and_then(|id| {
                documents
                    .iter()
                    .position(|document| document.document_id == *id)
            })
            .unwrap_or_else(|| {
                self.document_selected
                    .min(documents.len().saturating_sub(1))
            });
        self.documents = documents;
        self.sync_document_selection();
        Ok(())
    }

    async fn handle_key(
        &mut self,
        store: &V2Store,
        data_dir: &Path,
        terminal: &mut TerminalSession,
        key: KeyEvent,
    ) {
        if control_shortcut(key) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        self.notice = None;
        if control_shortcut(key) && key.code == KeyCode::Char('g') {
            if matches!(self.mode, Mode::Reader) {
                self.edit_document_body(store, terminal).await;
            } else {
                self.edit_focused_text(terminal);
            }
            return;
        }

        match &mut self.mode {
            Mode::Browse => self.handle_browse_key(store, data_dir, key).await,
            Mode::Reader => self.handle_reader_key(store, key).await,
            Mode::Search(input) => {
                if key.code == KeyCode::Esc {
                    self.mode = Mode::Browse;
                    return;
                }
                if key.code == KeyCode::Enter && command_key(key) {
                    let value = input.value();
                    let target = match self.workspace {
                        Workspace::Library => &mut self.query,
                        Workspace::Documents => &mut self.document_query,
                    };
                    let previous_query = std::mem::replace(target, value);
                    match self.refresh(store, None).await {
                        Ok(()) => self.mode = Mode::Browse,
                        Err(error) => {
                            match self.workspace {
                                Workspace::Library => self.query = previous_query,
                                Workspace::Documents => self.document_query = previous_query,
                            }
                            self.notice_error(error);
                        }
                    }
                    return;
                }
                input.handle_key(key, false);
            }
            Mode::Form(form) => {
                if key.code == KeyCode::Esc {
                    self.mode = Mode::Browse;
                    return;
                }
                if control_shortcut(key) && key.code == KeyCode::Char('s') {
                    self.submit_form(store, data_dir).await;
                    return;
                }
                form.handle_key(key);
            }
            Mode::DocumentForm(form) => {
                if key.code == KeyCode::Esc {
                    self.dismiss_overlay();
                    return;
                }
                if control_shortcut(key) && key.code == KeyCode::Char('s') {
                    self.submit_document_form(store).await;
                    return;
                }
                form.handle_key(key);
            }
            Mode::SyncSetup(form) => {
                if key.code == KeyCode::Esc {
                    self.mode = Mode::Browse;
                    return;
                }
                if control_shortcut(key) && key.code == KeyCode::Char('s') {
                    self.submit_sync_setup(data_dir);
                    return;
                }
                form.handle_key(key);
            }
            Mode::ConfirmDelete => match key.code {
                KeyCode::Enter | KeyCode::Char('y') if command_key(key) => {
                    match self.workspace {
                        Workspace::Library => self.delete_selected(store).await,
                        Workspace::Documents => self.delete_selected_document(store).await,
                    }
                }
                KeyCode::Char('n') if command_key(key) => self.dismiss_overlay(),
                KeyCode::Esc => self.dismiss_overlay(),
                _ => {}
            },
            Mode::ConfirmForceEnrich(target) => match key.code {
                KeyCode::Enter | KeyCode::Char('y') if command_key(key) => {
                    let target = target.clone();
                    self.mode = Mode::Browse;
                    self.force_enrich_selected(data_dir, target);
                }
                KeyCode::Char('n') if command_key(key) => self.mode = Mode::Browse,
                KeyCode::Esc => self.mode = Mode::Browse,
                _ => {}
            },
            Mode::Help => {
                if key.code == KeyCode::Esc
                    || command_key(key)
                        && matches!(key.code, KeyCode::Char('?') | KeyCode::Enter)
                {
                    self.dismiss_overlay();
                }
            }
        }
    }

    /// Closes an overlay onto whatever was underneath it.
    fn dismiss_overlay(&mut self) {
        self.mode = if self.workspace == Workspace::Documents && self.reader.is_some() {
            Mode::Reader
        } else {
            Mode::Browse
        };
    }

    fn accept_key_press(&mut self, key: KeyEvent) -> bool {
        let mode = self.mode.kind();
        let signature = (key.code, key.modifiers);
        if let Some((latched_mode, code, modifiers, observed_at)) = &mut self.action_latch
            && (*latched_mode, *code, *modifiers) == (mode, signature.0, signature.1)
        {
            *observed_at = Instant::now();
            return false;
        }
        self.action_latch = self
            .is_one_shot_key(key)
            .then(|| (mode, signature.0, signature.1, Instant::now()));
        true
    }

    fn release_key(&mut self, key: KeyEvent) {
        if self
            .action_latch
            .as_ref()
            .is_some_and(|(_, code, modifiers, _)| {
                (*code, *modifiers) == (key.code, key.modifiers)
            })
        {
            self.action_latch = None;
        }
    }

    fn note_key_repeat(&mut self, key: KeyEvent) {
        if let Some((_, code, modifiers, observed_at)) = &mut self.action_latch
            && (*code, *modifiers) == (key.code, key.modifiers)
        {
            *observed_at = Instant::now();
        }
    }

    fn clear_action_latch(&mut self) {
        if self
            .action_latch
            .as_ref()
            .is_some_and(|(_, _, _, observed_at)| observed_at.elapsed() >= ACTION_LATCH_IDLE)
        {
            self.action_latch = None;
        }
    }

    fn is_one_shot_key(&self, key: KeyEvent) -> bool {
        if control_shortcut(key) && matches!(key.code, KeyCode::Char('c' | 'g')) {
            return true;
        }
        match &self.mode {
            Mode::Browse => {
                control_shortcut(key) && key.code == KeyCode::Char('e')
                    || key.code == KeyCode::Esc && !self.active_query().is_empty()
                    || command_key(key)
                        && matches!(
                            key.code,
                            KeyCode::Char(
                                '?' | '/' | 'a' | 'e' | 'E' | 's' | ' ' | 'x' | 'r' | 'o'
                            ) | KeyCode::Enter
                                | KeyCode::Tab
                        )
            }
            Mode::Reader => {
                key.code == KeyCode::Esc
                    || control_shortcut(key) && key.code == KeyCode::Char('g')
                    || command_key(key)
                        && matches!(
                            key.code,
                            KeyCode::Char('q' | 'e' | ' ' | 't' | 'x' | 'r' | '?' | 's')
                                | KeyCode::Enter
                        )
            }
            Mode::Search(_) => {
                key.code == KeyCode::Esc || key.code == KeyCode::Enter && command_key(key)
            }
            Mode::Form(form) => {
                key.code == KeyCode::Esc
                    || control_shortcut(key) && key.code == KeyCode::Char('s')
                    || form.active >= form.fields.len()
                        && key.code == KeyCode::Char(' ')
                        && command_key(key)
            }
            Mode::DocumentForm(_) => {
                key.code == KeyCode::Esc
                    || control_shortcut(key) && key.code == KeyCode::Char('s')
            }
            Mode::SyncSetup(_) => {
                key.code == KeyCode::Esc
                    || control_shortcut(key) && key.code == KeyCode::Char('s')
            }
            Mode::ConfirmDelete => {
                key.code == KeyCode::Esc
                    || command_key(key)
                        && matches!(key.code, KeyCode::Enter | KeyCode::Char('y' | 'n'))
            }
            Mode::ConfirmForceEnrich(_) => {
                key.code == KeyCode::Esc
                    || command_key(key)
                        && matches!(key.code, KeyCode::Enter | KeyCode::Char('y' | 'n'))
            }
            Mode::Help => {
                key.code == KeyCode::Esc
                    || command_key(key)
                        && matches!(key.code, KeyCode::Char('?') | KeyCode::Enter)
            }
        }
    }

    fn handle_repeat_key(&mut self, key: KeyEvent) {
        self.notice = None;
        match &mut self.mode {
            Mode::Browse => match key.code {
                KeyCode::Down | KeyCode::Char('j') if command_key(key) => self.move_down(1),
                KeyCode::Up | KeyCode::Char('k') if command_key(key) => self.move_up(1),
                KeyCode::PageDown if command_key(key) => self.move_down(10),
                KeyCode::PageUp if command_key(key) => self.move_up(10),
                KeyCode::Char('d') if control_shortcut(key) => {
                    self.detail_scroll = self.detail_scroll.saturating_add(5);
                }
                KeyCode::Char('u') if control_shortcut(key) => {
                    self.detail_scroll = self.detail_scroll.saturating_sub(5);
                }
                _ => {}
            },
            Mode::Reader => self.move_reader(key),
            Mode::Search(_)
            | Mode::Form(_)
            | Mode::DocumentForm(_)
            | Mode::SyncSetup(_)
            | Mode::ConfirmDelete
            | Mode::ConfirmForceEnrich(_)
            | Mode::Help => {}
        }
    }

    async fn handle_browse_key(&mut self, store: &V2Store, data_dir: &Path, key: KeyEvent) {
        if key.code == KeyCode::Tab && command_key(key) {
            self.workspace = self.workspace.other();
            self.detail_scroll = 0;
            let _ = self.refresh_or_notice(store, None).await;
            return;
        }
        match self.workspace {
            Workspace::Library => self.handle_library_key(store, data_dir, key).await,
            Workspace::Documents => self.handle_documents_key(store, data_dir, key).await,
        }
    }

    fn active_query(&self) -> &str {
        match self.workspace {
            Workspace::Library => &self.query,
            Workspace::Documents => &self.document_query,
        }
    }

    fn move_down(&mut self, amount: usize) {
        match self.workspace {
            Workspace::Library => self.select_next(amount),
            Workspace::Documents => self.select_next_document(amount),
        }
    }

    fn move_up(&mut self, amount: usize) {
        match self.workspace {
            Workspace::Library => self.select_previous(amount),
            Workspace::Documents => self.select_previous_document(amount),
        }
    }

    async fn handle_library_key(&mut self, store: &V2Store, data_dir: &Path, key: KeyEvent) {
        if self.operation_blocks_mutations()
            && (command_key(key)
                && matches!(
                    key.code,
                    KeyCode::Char('a' | 'e' | 'E' | ' ' | 'x' | 'r') | KeyCode::Enter
                )
                || control_shortcut(key) && key.code == KeyCode::Char('e'))
        {
            self.notice = Some(Notice::info(
                "Wait for the initial sync connection to finish before changing this library",
            ));
            return;
        }
        match key.code {
            KeyCode::Char('q') if command_key(key) => self.should_quit = true,
            KeyCode::Char('?') if command_key(key) => self.mode = Mode::Help,
            KeyCode::Down | KeyCode::Char('j') if command_key(key) => self.select_next(1),
            KeyCode::Up | KeyCode::Char('k') if command_key(key) => self.select_previous(1),
            KeyCode::PageDown if command_key(key) => self.select_next(10),
            KeyCode::PageUp if command_key(key) => self.select_previous(10),
            KeyCode::Home | KeyCode::Char('g') if command_key(key) => self.select_first(),
            KeyCode::End | KeyCode::Char('G') if command_key(key) => self.select_last(),
            KeyCode::Char('/') if command_key(key) => {
                self.mode = Mode::Search(TextInput::new(self.query.clone()));
            }
            KeyCode::Esc if !self.query.is_empty() => {
                let previous_query = self.query.clone();
                self.query.clear();
                if !self.refresh_or_notice(store, None).await {
                    self.query = previous_query;
                }
            }
            KeyCode::Char('f') if command_key(key) => {
                self.favorite_only = !self.favorite_only;
                if !self.refresh_or_notice(store, None).await {
                    self.favorite_only = !self.favorite_only;
                }
            }
            KeyCode::Char('d') if control_shortcut(key) => {
                self.detail_scroll = self.detail_scroll.saturating_add(5);
            }
            KeyCode::Char('u') if control_shortcut(key) => {
                self.detail_scroll = self.detail_scroll.saturating_sub(5);
            }
            KeyCode::Char('d') if command_key(key) => {
                let previous = self.lifecycle;
                self.lifecycle = self.lifecycle.next();
                if !self.refresh_or_notice(store, None).await {
                    self.lifecycle = previous;
                }
            }
            KeyCode::Char('R') if command_key(key) => {
                let _ = self.refresh_or_notice(store, None).await;
            }
            KeyCode::Char('a') if command_key(key) => {
                self.mode = Mode::Form(Box::new(ItemForm::create()));
            }
            KeyCode::Char('e') | KeyCode::Enter if command_key(key) => {
                if let Some(item) = self.selected_item().cloned() {
                    self.mode = Mode::Form(Box::new(ItemForm::edit(item)));
                }
            }
            KeyCode::Char('E') if command_key(key) => {
                self.enrich_selected(data_dir);
            }
            KeyCode::Char('e') if control_shortcut(key) => {
                if self.operation.is_some() {
                    self.notice =
                        Some(Notice::info("Another network operation is still running"));
                } else if self
                    .selected_item()
                    .is_some_and(|item| item.state == "active")
                {
                    let item = self.selected_item().cloned().expect("selected active item");
                    match store.item_excerpt_text(&item.id).await {
                        Ok(expected_excerpt) => {
                            self.mode = Mode::ConfirmForceEnrich(ForceEnrichmentConfirmation {
                                item_id: item.id,
                                title: item.title,
                                expected_excerpt,
                            });
                        }
                        Err(error) => self.notice_error(error),
                    }
                }
            }
            KeyCode::Char('s') if command_key(key) => {
                if self.operation.is_some() {
                    self.notice =
                        Some(Notice::info("Another network operation is still running"));
                } else if self.status.sync_remote.is_some() {
                    self.synchronize(data_dir);
                } else {
                    self.mode = Mode::SyncSetup(SyncForm::new());
                }
            }
            KeyCode::Char(' ') if command_key(key) => self.toggle_favorite(store).await,
            KeyCode::Char('x') if command_key(key) => {
                if self
                    .selected_item()
                    .is_some_and(|item| item.state == "active")
                {
                    self.mode = Mode::ConfirmDelete;
                }
            }
            KeyCode::Char('r') if command_key(key) => self.restore_selected(store).await,
            _ => {}
        }
    }

    fn handle_paste(&mut self, text: String) {
        match &mut self.mode {
            Mode::Search(input) => input.insert_text(&single_line(&text)),
            Mode::Form(form) => form.insert_text(text),
            Mode::DocumentForm(form) => form.insert_text(text),
            Mode::SyncSetup(form) => form.insert_text(text),
            _ => {}
        }
    }

    fn edit_focused_text(&mut self, terminal: &mut TerminalSession) {
        let result = match &mut self.mode {
            Mode::Search(input) => edit_text_input(terminal, input, false),
            Mode::Form(form) if form.active < form.fields.len() => {
                let field = &mut form.fields[form.active];
                edit_text_input(terminal, &mut field.input, field.multiline)
            }
            Mode::DocumentForm(form) => {
                let field = &mut form.fields[form.active];
                edit_text_input(terminal, &mut field.input, field.multiline)
            }
            Mode::SyncSetup(form) => {
                edit_text_input(terminal, &mut form.fields[form.active].input, false)
            }
            Mode::Browse
            | Mode::Form(_)
            | Mode::Reader
            | Mode::ConfirmDelete
            | Mode::ConfirmForceEnrich(_)
            | Mode::Help => return,
        };
        if let Err(error) = result {
            self.notice_error(error);
        }
    }

    async fn submit_form(&mut self, store: &V2Store, data_dir: &Path) {
        let Mode::Form(form) = &self.mode else {
            return;
        };
        let submission = match form.submission() {
            Ok(submission) => submission,
            Err(error) => {
                self.notice_error(error);
                return;
            }
        };
        let result: TuiResult<(StoredItem, bool)> = match submission {
            FormSubmission::Create { request, enrich } => {
                if enrich {
                    let provider = match v2::configured_provider(data_dir) {
                        Ok(Some(provider)) => provider,
                        Ok(None) => {
                            self.notice_error(
                                "No enrichment provider is configured. Run `research enrich configure direct` or configure Firecrawl.",
                            );
                            return;
                        }
                        Err(error) => {
                            self.notice_error(error);
                            return;
                        }
                    };
                    store
                        .create_item_with_enrichment(request, provider)
                        .await
                        .map(|item| (item, true))
                        .map_err(Into::into)
                } else {
                    store
                        .create_item(request)
                        .await
                        .map(|item| (item, false))
                        .map_err(Into::into)
                }
            }
            FormSubmission::Edit(request) => store
                .edit_item(request)
                .await
                .map(|item| (item, false))
                .map_err(Into::into),
        };
        match result {
            Ok((item, enrich)) => {
                let item_id = item.id.clone();
                let action = if form.original.is_some() {
                    "Saved changes"
                } else {
                    "Captured save"
                };
                self.mode = Mode::Browse;
                self.notice = if enrich && self.operation.is_some() {
                    Some(Notice::info(
                        "Captured save; enrichment will run after the active network operation",
                    ))
                } else if enrich {
                    Some(Notice::info("Captured save; enriching metadata"))
                } else {
                    Some(Notice::info(action))
                };
                let _ = self.refresh_or_notice(store, Some(&item_id)).await;
                if enrich {
                    if self.operation.is_none() {
                        self.start_enrichment(data_dir, item_id, "Captured save", false, None);
                    } else {
                        self.queued_enrichment.push_back((item_id, "Captured save"));
                    }
                }
            }
            Err(error) => self.notice_error(error),
        }
    }

    fn enrich_selected(&mut self, data_dir: &Path) {
        if self.operation.is_some() {
            self.notice = Some(Notice::info("Another network operation is still running"));
            return;
        }
        let Some(item) = self.selected_item().cloned() else {
            return;
        };
        if item.state != "active" {
            self.notice = Some(Notice::info("Restore the save before enriching it"));
            return;
        }
        self.notice = Some(Notice::info("Enriching selected save"));
        self.start_enrichment(data_dir, item.id, "Enrichment", false, None);
    }

    fn force_enrich_selected(&mut self, data_dir: &Path, target: ForceEnrichmentConfirmation) {
        self.notice = Some(Notice::info("Re-enriching and replacing the excerpt"));
        self.start_enrichment(
            data_dir,
            target.item_id,
            "Excerpt replacement",
            true,
            Some(target.expected_excerpt),
        );
    }

    fn synchronize(&mut self, data_dir: &Path) {
        let data_dir = data_dir.to_path_buf();
        self.notice = Some(Notice::info("Synchronizing with GitHub"));
        self.operation = Some(BackgroundOperation {
            label: "syncing",
            blocks_mutations: false,
            handle: tokio::spawn(async move {
                let completion = match V2Store::open(&data_dir).await {
                    Ok(store) => match sync::run_once(&store).await {
                        Ok(result) => Notice::info(format!(
                            "Sync complete: {} downloaded, {} uploaded, {} pending",
                            result.downloaded, result.uploaded, result.pending
                        )),
                        Err(error) => Notice::error(error.to_string()),
                    },
                    Err(error) => Notice::error(error.to_string()),
                };
                BackgroundCompletion {
                    notice: completion,
                    preferred_id: None,
                }
            }),
        });
    }

    fn submit_sync_setup(&mut self, data_dir: &Path) {
        let Mode::SyncSetup(form) = &self.mode else {
            return;
        };
        let (repository, branch) = match form.submission() {
            Ok(submission) => submission,
            Err(error) => {
                self.notice_error(error);
                return;
            }
        };
        let data_dir = data_dir.to_path_buf();
        self.mode = Mode::Browse;
        self.notice = Some(Notice::info("Connecting private GitHub sync"));
        self.operation = Some(BackgroundOperation {
            label: "connecting sync",
            blocks_mutations: true,
            handle: tokio::spawn(async move {
                let completion = match V2Store::open(&data_dir).await {
                    Ok(store) => {
                        match sync::connect(&store, &repository, branch.as_deref()).await {
                            Ok(result) => Notice::info(format!(
                                "Connected and synced {}/{}: {} downloaded, {} uploaded",
                                result.remote.owner,
                                result.remote.repository,
                                result.cycle.downloaded,
                                result.cycle.uploaded
                            )),
                            Err(error) => Notice::error(error.to_string()),
                        }
                    }
                    Err(error) => Notice::error(error.to_string()),
                };
                BackgroundCompletion {
                    notice: completion,
                    preferred_id: None,
                }
            }),
        });
    }

    fn start_enrichment(
        &mut self,
        data_dir: &Path,
        item_id: String,
        action: &'static str,
        replace_excerpt: bool,
        expected_excerpt: Option<String>,
    ) {
        let data_dir = data_dir.to_path_buf();
        let preferred_id = item_id.clone();
        self.operation = Some(BackgroundOperation {
            label: "enriching",
            blocks_mutations: false,
            handle: tokio::spawn(async move {
                let notice = match V2Store::open(&data_dir).await {
                    Ok(store) => {
                        let result = if action == "Captured save" {
                            v2::attempt_queued_enrichment(&store, &data_dir, &item_id)
                                .await
                                .and_then(|outcome| {
                                    outcome.ok_or_else(|| {
                                        io::Error::other("the enrichment job disappeared")
                                            .into()
                                    })
                                })
                        } else {
                            v2::enrich_item_with_configured_provider(
                                &store,
                                &data_dir,
                                &item_id,
                                replace_excerpt,
                                expected_excerpt,
                            )
                            .await
                        };
                        match result {
                            Ok(outcome) => enrichment_notice(action, &outcome),
                            Err(error) if action == "Captured save" => Notice::info(format!(
                                "Captured save; metadata enrichment remains queued ({error})"
                            )),
                            Err(error) => Notice::error(error.to_string()),
                        }
                    }
                    Err(error) if action == "Captured save" => Notice::info(format!(
                        "Captured save; metadata enrichment remains queued ({error})"
                    )),
                    Err(error) => Notice::error(error.to_string()),
                };
                BackgroundCompletion {
                    notice,
                    preferred_id: Some(preferred_id),
                }
            }),
        });
    }

    fn operation_finished(&self) -> bool {
        self.operation
            .as_ref()
            .is_some_and(|operation| operation.handle.is_finished())
    }

    fn operation_blocks_mutations(&self) -> bool {
        self.operation
            .as_ref()
            .is_some_and(|operation| operation.blocks_mutations)
    }

    async fn finish_operation(&mut self, store: &V2Store, data_dir: &Path) {
        let Some(operation) = self.operation.take() else {
            return;
        };
        match operation.handle.await {
            Ok(completion) => {
                self.notice = Some(completion.notice);
                let _ = self
                    .refresh_or_notice(store, completion.preferred_id.as_deref())
                    .await;
            }
            Err(_) => self.notice_error("The background operation stopped unexpectedly"),
        }
        if let Some((item_id, action)) = self.queued_enrichment.pop_front() {
            self.start_enrichment(data_dir, item_id, action, false, None);
        }
    }

    async fn toggle_favorite(&mut self, store: &V2Store) {
        let Some(item) = self.selected_item().cloned() else {
            return;
        };
        let item_id = item.id.clone();
        let result = store
            .edit_item(EditItemRequest {
                item_id: item.id,
                favorite: Some(!item.favorite),
                ..EditItemRequest::default()
            })
            .await;
        match result {
            Ok(_) => {
                self.notice = Some(Notice::info(if item.favorite {
                    "Removed favorite"
                } else {
                    "Marked favorite"
                }));
                let _ = self.refresh_or_notice(store, Some(&item_id)).await;
            }
            Err(error) => self.notice_error(error),
        }
    }

    async fn delete_selected(&mut self, store: &V2Store) {
        let Some(item_id) = self.selected_item().map(|item| item.id.clone()) else {
            self.mode = Mode::Browse;
            return;
        };
        match store.delete_item(&item_id).await {
            Ok(_) => {
                self.mode = Mode::Browse;
                self.notice = Some(Notice::info("Moved save to deleted"));
                let _ = self.refresh_or_notice(store, None).await;
            }
            Err(error) => self.notice_error(error),
        }
    }

    async fn restore_selected(&mut self, store: &V2Store) {
        let Some(item) = self.selected_item().cloned() else {
            return;
        };
        if item.state != "deleted" {
            self.notice = Some(Notice::info("Selected save is already active"));
            return;
        }
        let item_id = item.id.clone();
        match store.restore_item(&item_id).await {
            Ok(_) => {
                self.notice = Some(Notice::info("Restored save"));
                let _ = self.refresh_or_notice(store, Some(&item_id)).await;
            }
            Err(error) => self.notice_error(error),
        }
    }

    /// Keys for the documents workspace.
    ///
    /// It intentionally reads like the library: the same navigation, the same
    /// lifecycle keys, the same search key. Only what a document actually has
    /// differs — Enter reads it rather than editing it, and there is no
    /// favorite, enrichment, or URL.
    async fn handle_documents_key(&mut self, store: &V2Store, data_dir: &Path, key: KeyEvent) {
        if self.operation_blocks_mutations()
            && command_key(key)
            && matches!(key.code, KeyCode::Char('a' | 'e' | 'x' | 'r'))
        {
            self.notice = Some(Notice::info(
                "Wait for the initial sync connection to finish before changing this library",
            ));
            return;
        }
        match key.code {
            KeyCode::Char('q') if command_key(key) => self.should_quit = true,
            KeyCode::Char('?') if command_key(key) => self.mode = Mode::Help,
            KeyCode::Down | KeyCode::Char('j') if command_key(key) => {
                self.select_next_document(1);
            }
            KeyCode::Up | KeyCode::Char('k') if command_key(key) => {
                self.select_previous_document(1);
            }
            KeyCode::PageDown if command_key(key) => self.select_next_document(10),
            KeyCode::PageUp if command_key(key) => self.select_previous_document(10),
            KeyCode::Home | KeyCode::Char('g') if command_key(key) => {
                self.select_first_document();
            }
            KeyCode::End | KeyCode::Char('G') if command_key(key) => {
                self.select_last_document();
            }
            KeyCode::Char('d') if control_shortcut(key) => {
                self.detail_scroll = self.detail_scroll.saturating_add(5);
            }
            KeyCode::Char('u') if control_shortcut(key) => {
                self.detail_scroll = self.detail_scroll.saturating_sub(5);
            }
            KeyCode::Char('/') if command_key(key) => {
                self.mode = Mode::Search(TextInput::new(self.document_query.clone()));
            }
            KeyCode::Esc if !self.document_query.is_empty() => {
                let previous_query = self.document_query.clone();
                self.document_query.clear();
                if !self.refresh_or_notice(store, None).await {
                    self.document_query = previous_query;
                }
            }
            KeyCode::Char('d') if command_key(key) => {
                let previous = self.document_lifecycle;
                self.document_lifecycle = self.document_lifecycle.next();
                if !self.refresh_or_notice(store, None).await {
                    self.document_lifecycle = previous;
                }
            }
            KeyCode::Char('R') if command_key(key) => {
                let _ = self.refresh_or_notice(store, None).await;
            }
            KeyCode::Char('o') | KeyCode::Enter if command_key(key) => {
                self.open_selected_document(store).await;
            }
            KeyCode::Char('a') if command_key(key) => {
                self.mode = Mode::DocumentForm(Box::new(DocumentForm::create()));
            }
            KeyCode::Char('e') if command_key(key) => {
                self.edit_selected_document(store).await;
            }
            KeyCode::Char('s') if command_key(key) => {
                if self.operation.is_some() {
                    self.notice =
                        Some(Notice::info("Another network operation is still running"));
                } else if self.status.sync_remote.is_some() {
                    self.synchronize(data_dir);
                } else {
                    self.mode = Mode::SyncSetup(SyncForm::new());
                }
            }
            KeyCode::Char('x') if command_key(key) => {
                if self
                    .selected_document()
                    .is_some_and(|document| document.lifecycle_state == LifecycleState::Active)
                {
                    self.mode = Mode::ConfirmDelete;
                }
            }
            KeyCode::Char('r') if command_key(key) => {
                self.restore_selected_document(store).await
            }
            _ => {}
        }
    }

    /// Keys for the open document.
    async fn handle_reader_key(&mut self, store: &V2Store, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') if command_key(key) => {
                self.reader = None;
                self.mode = Mode::Browse;
            }
            KeyCode::Char('?') if command_key(key) => self.mode = Mode::Help,
            KeyCode::Char('e') | KeyCode::Enter if command_key(key) => {
                self.edit_open_document();
            }
            KeyCode::Char(' ' | 't') if command_key(key) => {
                self.toggle_open_todo(store).await;
            }
            KeyCode::Char('x') if command_key(key) => {
                if self.reader.as_ref().is_some_and(|reader| !reader.deleted) {
                    self.mode = Mode::ConfirmDelete;
                }
            }
            KeyCode::Char('r') if command_key(key) => {
                self.restore_selected_document(store).await;
            }
            KeyCode::Char('R') if command_key(key) => self.reload_reader(store).await,
            _ => self.move_reader(key),
        }
    }

    /// Cursor movement in the reader. The visible window follows at render
    /// time, so movement stays independent of how the body happens to wrap.
    fn move_reader(&mut self, key: KeyEvent) {
        let Some(reader) = &mut self.reader else {
            return;
        };
        match key.code {
            KeyCode::Down | KeyCode::Char('j') if command_key(key) => reader.move_cursor(1),
            KeyCode::Up | KeyCode::Char('k') if command_key(key) => reader.move_cursor(-1),
            KeyCode::PageDown if command_key(key) => reader.move_cursor(15),
            KeyCode::PageUp if command_key(key) => reader.move_cursor(-15),
            KeyCode::Char('d') if control_shortcut(key) => reader.move_cursor(10),
            KeyCode::Char('u') if control_shortcut(key) => reader.move_cursor(-10),
            KeyCode::Home | KeyCode::Char('g') if command_key(key) => reader.cursor = 0,
            KeyCode::End | KeyCode::Char('G') if command_key(key) => reader.cursor_to_last(),
            _ => {}
        }
    }

    /// Opens the selected document. This is the only path that reads a body.
    async fn open_selected_document(&mut self, store: &V2Store) {
        let Some(document_id) = self
            .selected_document()
            .map(|document| document.document_id.clone())
        else {
            return;
        };
        match load_reader(store, &document_id).await {
            Ok(reader) => {
                self.reader = Some(reader);
                self.mode = Mode::Reader;
            }
            Err(error) => self.notice_error(error),
        }
    }

    async fn reload_reader(&mut self, store: &V2Store) {
        let Some(document_id) = self
            .reader
            .as_ref()
            .map(|reader| reader.document_id.clone())
        else {
            return;
        };
        match load_reader(store, &document_id).await {
            Ok(mut reader) => {
                if let Some(previous) = &self.reader {
                    reader.cursor = previous.cursor.min(reader.line_count().saturating_sub(1));
                }
                self.reader = Some(reader);
                self.notice = Some(Notice::info("Reloaded document"));
            }
            Err(error) => self.notice_error(error),
        }
    }

    /// Opens the edit form for the selected document, loading its body.
    async fn edit_selected_document(&mut self, store: &V2Store) {
        let Some(document_id) = self
            .selected_document()
            .map(|document| document.document_id.clone())
        else {
            return;
        };
        match store.zen_document(&document_id).await {
            Ok(view) => {
                self.mode = Mode::DocumentForm(Box::new(DocumentForm::edit(&view)));
            }
            Err(error) => self.notice_error(error),
        }
    }

    /// Opens the edit form over the reader, reusing the body already in hand.
    fn edit_open_document(&mut self) {
        let Some(reader) = &self.reader else {
            return;
        };
        self.mode = Mode::DocumentForm(Box::new(DocumentForm::from_reader(reader)));
    }

    async fn submit_document_form(&mut self, store: &V2Store) {
        let Mode::DocumentForm(form) = &self.mode else {
            return;
        };
        let submission = match form.submission() {
            Ok(submission) => submission,
            Err(error) => {
                self.notice_error(error);
                return;
            }
        };
        match submission {
            DocumentSubmission::Create(request) => {
                match store.create_zen_document(request).await {
                    Ok(summary) => {
                        let document_id = summary.document_id.clone();
                        self.mode = Mode::Browse;
                        self.reader = None;
                        self.notice = Some(Notice::info("Created document"));
                        let _ = self.refresh_or_notice(store, None).await;
                        let _ = self.refresh_documents(store, Some(&document_id)).await;
                    }
                    Err(error) => self.notice_error(error),
                }
            }
            DocumentSubmission::Edit(request) => {
                let document_id = request.document_id.clone();
                match store.edit_zen_document(request).await {
                    Ok(_) => {
                        if self.reader.is_some() {
                            self.reload_reader(store).await;
                        }
                        self.notice = Some(Notice::info("Saved document"));
                        self.dismiss_overlay();
                        let _ = self.refresh_or_notice(store, None).await;
                        let _ = self.refresh_documents(store, Some(&document_id)).await;
                    }
                    Err(StoreError::NoChanges) => {
                        self.dismiss_overlay();
                        self.notice = Some(Notice::info("No changes to save"));
                    }
                    Err(StoreError::StaleEdit) => self.notice_error(
                        "This document changed elsewhere; press Esc and reopen it before saving",
                    ),
                    Err(error) => self.notice_error(error),
                }
            }
        }
    }

    /// Toggles the checkbox on the cursor line.
    ///
    /// The store splices only the character that changed, so two devices can
    /// tick different boxes in the same document and keep both.
    async fn toggle_open_todo(&mut self, store: &V2Store) {
        if self.operation_blocks_mutations() {
            self.notice = Some(Notice::info(
                "Wait for the initial sync connection to finish before changing this library",
            ));
            return;
        }
        let Some(reader) = &self.reader else {
            return;
        };
        if reader.deleted {
            self.notice = Some(Notice::info("Restore the document before editing it"));
            return;
        }
        let Some(body) = reader.toggled_body() else {
            self.notice = Some(Notice::info("No task list item on this line"));
            return;
        };
        self.write_open_body(store, body, "Toggled task").await;
    }

    /// Opens the whole body in the configured editor and saves what comes back.
    async fn edit_document_body(&mut self, store: &V2Store, terminal: &mut TerminalSession) {
        let Some(reader) = &self.reader else {
            return;
        };
        if reader.deleted {
            self.notice = Some(Notice::info("Restore the document before editing it"));
            return;
        }
        let mut input = TextInput::new(reader.body().to_owned());
        if let Err(error) = edit_text_input(terminal, &mut input, true) {
            self.notice_error(error);
            return;
        }
        let body = input.value();
        if self
            .reader
            .as_ref()
            .is_some_and(|reader| reader.body() == body)
        {
            self.notice = Some(Notice::info("No changes to save"));
            return;
        }
        self.write_open_body(store, body, "Saved document").await;
    }

    /// Writes a new body for the open document, refusing a stale replacement.
    async fn write_open_body(&mut self, store: &V2Store, body: String, action: &'static str) {
        let Some(reader) = &self.reader else {
            return;
        };
        let document_id = reader.document_id.clone();
        let result = store
            .edit_zen_document(EditZenDocumentRequest {
                document_id: document_id.clone(),
                body: Some(body.clone()),
                expected_body: Some(reader.body().to_owned()),
                ..EditZenDocumentRequest::default()
            })
            .await;
        match result {
            Ok(_) => {
                let mentions = resolve_mentions(store, &body).await;
                if let Some(reader) = &mut self.reader {
                    reader.set_body(body);
                    reader.set_mentions(mentions);
                }
                self.notice = Some(Notice::info(action));
                let _ = self.refresh_or_notice(store, None).await;
                let _ = self.refresh_documents(store, Some(&document_id)).await;
            }
            Err(StoreError::NoChanges) => {
                self.notice = Some(Notice::info("No changes to save"));
            }
            // The buffer is the only copy of that work, so it is kept on disk
            // rather than discarded with the error.
            Err(StoreError::StaleEdit) => match rescue_body(&body) {
                Ok(path) => self.notice_error(format!(
                    "This document changed elsewhere; your version is saved at {}",
                    path.display()
                )),
                Err(error) => self.notice_error(format!(
                    "This document changed elsewhere and the rejected text could not be saved: {error}"
                )),
            },
            Err(error) => self.notice_error(error),
        }
    }

    async fn delete_selected_document(&mut self, store: &V2Store) {
        let Some(document_id) = self
            .reader
            .as_ref()
            .map(|reader| reader.document_id.clone())
            .or_else(|| {
                self.selected_document()
                    .map(|document| document.document_id.clone())
            })
        else {
            self.mode = Mode::Browse;
            return;
        };
        match store.delete_zen_document(&document_id).await {
            Ok(_) => {
                self.reader = None;
                self.mode = Mode::Browse;
                self.notice = Some(Notice::info("Moved document to deleted"));
                let _ = self.refresh_or_notice(store, None).await;
            }
            Err(error) => self.notice_error(error),
        }
    }

    async fn restore_selected_document(&mut self, store: &V2Store) {
        let Some(document_id) = self
            .reader
            .as_ref()
            .filter(|reader| reader.deleted)
            .map(|reader| reader.document_id.clone())
            .or_else(|| {
                self.selected_document()
                    .filter(|document| document.lifecycle_state == LifecycleState::Deleted)
                    .map(|document| document.document_id.clone())
            })
        else {
            self.notice = Some(Notice::info("Selected document is already active"));
            return;
        };
        match store.restore_zen_document(&document_id).await {
            Ok(_) => {
                self.notice = Some(Notice::info("Restored document"));
                if self.reader.is_some() {
                    self.reload_reader(store).await;
                }
                let _ = self.refresh_or_notice(store, None).await;
                let _ = self.refresh_documents(store, Some(&document_id)).await;
            }
            Err(error) => self.notice_error(error),
        }
    }

    async fn refresh_or_notice(&mut self, store: &V2Store, preferred_id: Option<&str>) -> bool {
        match self.refresh(store, preferred_id).await {
            Ok(()) => true,
            Err(error) => {
                self.notice_error(error);
                false
            }
        }
    }

    fn notice_error(&mut self, error: impl std::fmt::Display) {
        self.notice = Some(Notice::error(error.to_string()));
    }

    fn selected_item(&self) -> Option<&StoredItem> {
        self.items.get(self.selected)
    }

    fn selected_document(&self) -> Option<&ZenDocumentSummary> {
        self.documents.get(self.document_selected)
    }

    fn sync_document_selection(&mut self) {
        self.document_list_state
            .select((!self.documents.is_empty()).then_some(self.document_selected));
    }

    fn select_next_document(&mut self, amount: usize) {
        if !self.documents.is_empty() {
            self.document_selected = self
                .document_selected
                .saturating_add(amount)
                .min(self.documents.len() - 1);
            self.detail_scroll = 0;
            self.sync_document_selection();
        }
    }

    fn select_first_document(&mut self) {
        self.document_selected = 0;
        self.detail_scroll = 0;
        self.sync_document_selection();
    }

    fn select_last_document(&mut self) {
        self.document_selected = self.documents.len().saturating_sub(1);
        self.detail_scroll = 0;
        self.sync_document_selection();
    }

    fn select_previous_document(&mut self, amount: usize) {
        self.document_selected = self.document_selected.saturating_sub(amount);
        self.detail_scroll = 0;
        self.sync_document_selection();
    }

    fn select_next(&mut self, amount: usize) {
        if !self.items.is_empty() {
            self.selected = (self.selected + amount).min(self.items.len() - 1);
            self.detail_scroll = 0;
            self.sync_selection();
        }
    }

    fn select_previous(&mut self, amount: usize) {
        self.selected = self.selected.saturating_sub(amount);
        self.detail_scroll = 0;
        self.sync_selection();
    }

    fn select_first(&mut self) {
        self.selected = 0;
        self.detail_scroll = 0;
        self.sync_selection();
    }

    fn select_last(&mut self) {
        self.selected = self.items.len().saturating_sub(1);
        self.detail_scroll = 0;
        self.sync_selection();
    }

    fn sync_selection(&mut self) {
        self.list_state
            .select((!self.items.is_empty()).then_some(self.selected));
    }
}

/// The two things a library holds: saved URLs, and authored documents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Workspace {
    Library,
    Documents,
}

impl Workspace {
    fn other(self) -> Self {
        match self {
            Self::Library => Self::Documents,
            Self::Documents => Self::Library,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Library => "saves",
            Self::Documents => "documents",
        }
    }
}

/// Case-insensitive match over the metadata the index already holds.
///
/// Bodies are deliberately absent: filtering must not be a reason to read one.
fn document_matches(document: &ZenDocumentSummary, needle: &str) -> bool {
    document
        .title
        .as_deref()
        .is_some_and(|title| title.to_lowercase().contains(needle))
        || document
            .tags
            .iter()
            .any(|tag| tag.to_lowercase().contains(needle))
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum LifecycleFilter {
    Active,
    All,
    Deleted,
}

impl LifecycleFilter {
    fn next(self) -> Self {
        match self {
            Self::Active => Self::All,
            Self::All => Self::Deleted,
            Self::Deleted => Self::Active,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::All => "all",
            Self::Deleted => "deleted",
        }
    }
}

enum Mode {
    Browse,
    Search(TextInput),
    Form(Box<ItemForm>),
    /// Title, body, and tags of one document. The body is a buffer, so every
    /// save carries the text it started from.
    DocumentForm(Box<DocumentForm>),
    /// Reading the open document. The body lives in `App::reader`.
    Reader,
    SyncSetup(SyncForm),
    ConfirmDelete,
    ConfirmForceEnrich(ForceEnrichmentConfirmation),
    Help,
}

impl Mode {
    fn kind(&self) -> ModeKind {
        match self {
            Self::Browse => ModeKind::Browse,
            Self::Search(_) => ModeKind::Search,
            Self::Form(_) => ModeKind::Form,
            Self::DocumentForm(_) => ModeKind::DocumentForm,
            Self::Reader => ModeKind::Reader,
            Self::SyncSetup(_) => ModeKind::SyncSetup,
            Self::ConfirmDelete => ModeKind::ConfirmDelete,
            Self::ConfirmForceEnrich(_) => ModeKind::ConfirmForceEnrich,
            Self::Help => ModeKind::Help,
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ModeKind {
    Browse,
    Search,
    Form,
    DocumentForm,
    Reader,
    SyncSetup,
    ConfirmDelete,
    ConfirmForceEnrich,
    Help,
}

struct Notice {
    text: String,
    error: bool,
}

impl Notice {
    fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: true,
        }
    }
}

struct BackgroundOperation {
    label: &'static str,
    blocks_mutations: bool,
    handle: JoinHandle<BackgroundCompletion>,
}

struct BackgroundCompletion {
    notice: Notice,
    preferred_id: Option<String>,
}

#[derive(Clone)]
struct ForceEnrichmentConfirmation {
    item_id: String,
    title: Option<String>,
    expected_excerpt: String,
}

struct ItemForm {
    fields: Vec<FormField>,
    active: usize,
    favorite: bool,
    enrich: bool,
    original: Option<StoredItem>,
}

impl ItemForm {
    fn create() -> Self {
        Self {
            fields: vec![
                FormField::new("URL", "", false),
                FormField::new("Title", "", false),
                FormField::new("Excerpt", "", true),
                FormField::new("Note", "", true),
                FormField::new("Tags (comma list or JSON)", "", false),
            ],
            active: 0,
            favorite: false,
            enrich: false,
            original: None,
        }
    }

    fn edit(item: StoredItem) -> Self {
        Self {
            fields: vec![
                FormField::new("URL", &item.url, false),
                FormField::new("Title", item.title.as_deref().unwrap_or_default(), false),
                FormField::new("Excerpt", item.excerpt.as_deref().unwrap_or_default(), true),
                FormField::new("Note", item.note.as_deref().unwrap_or_default(), true),
                FormField::new("Tags (comma list or JSON)", &format_tags(&item.tags), false),
            ],
            active: 0,
            favorite: item.favorite,
            enrich: false,
            original: Some(item),
        }
    }

    fn title(&self) -> &'static str {
        if self.original.is_some() {
            "Edit save"
        } else {
            "Capture save"
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(' ') if self.active == self.fields.len() && command_key(key) => {
                self.favorite = !self.favorite;
            }
            KeyCode::Char(' ')
                if self.can_enrich()
                    && self.active == self.fields.len() + 1
                    && command_key(key) =>
            {
                self.enrich = !self.enrich;
            }
            _ => {
                let focusable = self.focusable_count();
                handle_form_field_key(&mut self.fields, &mut self.active, focusable, key);
            }
        }
    }

    fn can_enrich(&self) -> bool {
        self.original.is_none()
    }

    fn focusable_count(&self) -> usize {
        self.fields.len() + 1 + usize::from(self.can_enrich())
    }

    fn insert_text(&mut self, text: String) {
        if self.active >= self.fields.len() {
            return;
        }
        let value = if self.fields[self.active].multiline {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            single_line(&text)
        };
        self.fields[self.active].input.insert_text(&value);
    }

    fn submission(&self) -> Result<FormSubmission, String> {
        let url = self.fields[0].input.value().trim().to_owned();
        let title = self.fields[1].input.value().to_owned();
        let excerpt = self.fields[2].input.value().to_owned();
        let note = self.fields[3].input.value().to_owned();
        let tags = parse_tags(&self.fields[4].input.value())?;

        let Some(original) = &self.original else {
            return Ok(FormSubmission::Create {
                request: CreateItemRequest {
                    url,
                    title: nonempty(title),
                    excerpt: nonempty(excerpt),
                    favorite: self.favorite,
                    language: None,
                    saved_at: None,
                    note,
                    tags,
                },
                enrich: self.enrich,
            });
        };

        let old_tags = original.tags.iter().cloned().collect::<BTreeSet<_>>();
        let new_tags = tags.into_iter().collect::<BTreeSet<_>>();
        let original_note = original.note.as_deref().unwrap_or_default();
        let note_changed = note != original_note;
        Ok(FormSubmission::Edit(EditItemRequest {
            item_id: original.id.clone(),
            url: (url != original.url).then_some(url),
            title: optional_text_change(original.title.as_deref(), &title),
            excerpt: optional_text_change(original.excerpt.as_deref(), &excerpt),
            favorite: (self.favorite != original.favorite).then_some(self.favorite),
            language: None,
            saved_at: None,
            note: note_changed.then_some(note),
            expected_note: note_changed.then(|| original_note.to_owned()),
            add_tags: new_tags.difference(&old_tags).cloned().collect(),
            remove_tags: old_tags.difference(&new_tags).cloned().collect(),
        }))
    }
}

enum FormSubmission {
    Create {
        request: CreateItemRequest,
        enrich: bool,
    },
    Edit(EditItemRequest),
}

/// Title, body, and tags of one document.
struct DocumentForm {
    fields: Vec<FormField>,
    active: usize,
    original: Option<DocumentOriginal>,
}

/// What an edit form opened with, so a save can send exactly that back as its
/// precondition instead of trusting the buffer.
struct DocumentOriginal {
    document_id: String,
    title: Option<String>,
    body: String,
    tags: Vec<String>,
}

impl DocumentForm {
    fn create() -> Self {
        Self {
            fields: vec![
                FormField::new("Title", "", false),
                FormField::new("Body (Markdown, task lists, mentions)", "", true),
                FormField::new("Tags (comma list or JSON)", "", false),
            ],
            active: 0,
            original: None,
        }
    }

    fn edit(view: &research_domain::ZenDocumentView) -> Self {
        Self::from_parts(
            &view.document_id,
            view.title.value.as_deref(),
            &view.body,
            &view.tags,
        )
    }

    fn from_reader(reader: &Reader) -> Self {
        Self::from_parts(
            &reader.document_id,
            reader.title.as_deref(),
            reader.body(),
            &reader.tags,
        )
    }

    fn from_parts(document_id: &str, title: Option<&str>, body: &str, tags: &[String]) -> Self {
        Self {
            fields: vec![
                FormField::new("Title", title.unwrap_or_default(), false),
                FormField::new("Body (Markdown, task lists, mentions)", body, true),
                FormField::new("Tags (comma list or JSON)", &format_tags(tags), false),
            ],
            active: 0,
            original: Some(DocumentOriginal {
                document_id: document_id.to_owned(),
                title: title.map(str::to_owned),
                body: body.to_owned(),
                tags: tags.to_vec(),
            }),
        }
    }

    fn title(&self) -> &'static str {
        if self.original.is_some() {
            "Edit document"
        } else {
            "New document"
        }
    }

    fn body_bytes(&self) -> usize {
        self.fields[1].input.value().len()
    }

    fn handle_key(&mut self, key: KeyEvent) {
        let focusable = self.fields.len();
        handle_form_field_key(&mut self.fields, &mut self.active, focusable, key);
    }

    fn insert_text(&mut self, text: String) {
        let value = if self.fields[self.active].multiline {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            single_line(&text)
        };
        self.fields[self.active].input.insert_text(&value);
    }

    fn submission(&self) -> Result<DocumentSubmission, String> {
        let title = self.fields[0].input.value().trim().to_owned();
        let body = self.fields[1].input.value();
        let tags = parse_tags(&self.fields[2].input.value())?;
        if body.len() > MAX_ZEN_BODY_BYTES {
            return Err(format!(
                "the body is {} and the limit is {}",
                format_bytes(body.len()),
                format_bytes(MAX_ZEN_BODY_BYTES)
            ));
        }

        let Some(original) = &self.original else {
            return Ok(DocumentSubmission::Create(CreateZenDocumentRequest {
                title: nonempty(title),
                body,
                tags,
            }));
        };

        let old_tags = original.tags.iter().cloned().collect::<BTreeSet<_>>();
        let new_tags = tags.into_iter().collect::<BTreeSet<_>>();
        let title_changed = nonempty(title.clone()) != original.title;
        let body_changed = body != original.body;
        Ok(DocumentSubmission::Edit(EditZenDocumentRequest {
            document_id: original.document_id.clone(),
            title: title_changed.then(|| nonempty(title)),
            body: body_changed.then_some(body),
            expected_body: body_changed.then(|| original.body.clone()),
            add_tags: new_tags.difference(&old_tags).cloned().collect(),
            remove_tags: old_tags.difference(&new_tags).cloned().collect(),
        }))
    }
}

enum DocumentSubmission {
    Create(CreateZenDocumentRequest),
    Edit(EditZenDocumentRequest),
}

/// Field navigation and text entry shared by every form.
fn handle_form_field_key(
    fields: &mut [FormField],
    active: &mut usize,
    focusable: usize,
    key: KeyEvent,
) {
    match key.code {
        KeyCode::Tab | KeyCode::Enter if command_key(key) => {
            *active = (*active + 1) % focusable;
        }
        KeyCode::BackTab if command_key(key) => {
            *active = active.checked_sub(1).unwrap_or(focusable - 1);
        }
        KeyCode::Down | KeyCode::Up
            if command_key(key) && *active < fields.len() && fields[*active].multiline =>
        {
            fields[*active].input.handle_key(key, true);
        }
        KeyCode::Down if command_key(key) => *active = (*active + 1) % focusable,
        KeyCode::Up if command_key(key) => {
            *active = active.checked_sub(1).unwrap_or(focusable - 1);
        }
        KeyCode::Char('j' | 'n')
            if control_shortcut(key) && *active < fields.len() && fields[*active].multiline =>
        {
            fields[*active].input.insert_char('\n');
        }
        _ if *active < fields.len() => {
            let multiline = fields[*active].multiline;
            fields[*active].input.handle_key(key, multiline);
        }
        _ => {}
    }
}

/// Reads one document and resolves the mentions its body makes.
async fn load_reader(store: &V2Store, document_id: &str) -> TuiResult<Reader> {
    let view = store.zen_document(document_id).await?;
    let mentions = resolve_mentions(store, &view.body).await;
    Ok(Reader::new(view, mentions))
}

/// Resolves `research:item/<uuid>` mentions against the local projection.
///
/// Nothing here is written back: the map is derived at view time and discarded
/// with the reader, which is what keeps mentions one-way references.
async fn resolve_mentions(store: &V2Store, body: &str) -> Mentions {
    let mut mentions = Mentions::new();
    for item_id in mention_ids(body) {
        let target = match store.item(&item_id).await {
            Ok(item) if item.state == "deleted" => MentionTarget::Deleted { title: item.title },
            Ok(item) => MentionTarget::Active {
                title: item.title,
                url: item.url,
            },
            Err(_) => MentionTarget::Unresolved,
        };
        mentions.insert(item_id, target);
    }
    mentions
}

/// Keeps a rejected body where its author can find it again.
fn rescue_body(body: &str) -> io::Result<std::path::PathBuf> {
    let mut file = tempfile::Builder::new()
        .prefix("researchpocket-rejected-")
        .suffix(".md")
        .tempfile()?;
    file.write_all(body.as_bytes())?;
    file.flush()?;
    file.into_temp_path()
        .keep()
        .map_err(|error| io::Error::other(error.to_string()))
}

struct SyncForm {
    fields: Vec<FormField>,
    active: usize,
}

impl SyncForm {
    fn new() -> Self {
        Self {
            fields: vec![
                FormField::new("Private repository (OWNER/NAME)", "", false),
                FormField::new("Branch (optional)", "", false),
            ],
            active: 0,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Tab | KeyCode::Down | KeyCode::Enter if command_key(key) => {
                self.active = (self.active + 1) % self.fields.len();
            }
            KeyCode::BackTab | KeyCode::Up if command_key(key) => {
                self.active = self.active.checked_sub(1).unwrap_or(self.fields.len() - 1);
            }
            _ => self.fields[self.active].input.handle_key(key, false),
        }
    }

    fn insert_text(&mut self, text: String) {
        self.fields[self.active]
            .input
            .insert_text(&single_line(&text));
    }

    fn submission(&self) -> Result<(String, Option<String>), String> {
        let repository = self.fields[0].input.value().trim().to_owned();
        if repository.is_empty() {
            return Err("Enter a private GitHub repository as OWNER/NAME".to_owned());
        }
        let branch = self.fields[1].input.value().trim().to_owned();
        Ok((repository, (!branch.is_empty()).then_some(branch)))
    }
}

struct FormField {
    label: &'static str,
    input: TextInput,
    multiline: bool,
}

impl FormField {
    fn new(label: &'static str, value: &str, multiline: bool) -> Self {
        Self {
            label,
            input: TextInput::new(value.to_owned()),
            multiline,
        }
    }
}

struct TextInput {
    chars: Vec<char>,
    cursor: usize,
    vertical_column: Option<usize>,
}

impl TextInput {
    fn new(value: String) -> Self {
        let chars = value.chars().collect::<Vec<_>>();
        let cursor = chars.len();
        Self {
            chars,
            cursor,
            vertical_column: None,
        }
    }

    fn value(&self) -> String {
        self.chars.iter().collect()
    }

    fn replace(&mut self, value: String) {
        self.chars = value.chars().collect();
        self.cursor = self.chars.len();
        self.vertical_column = None;
    }

    fn display_value(&self, focused: bool, max_width: usize) -> (String, u16) {
        let cursor_space = usize::from(focused);
        let rendered_width = UnicodeWidthStr::width(self.value().as_str()) + cursor_space;
        if rendered_width <= max_width.max(1) {
            return (
                self.value(),
                u16::try_from(UnicodeWidthStr::width(
                    self.chars[..self.cursor]
                        .iter()
                        .collect::<String>()
                        .as_str(),
                ))
                .unwrap_or(u16::MAX),
            );
        }

        let capacity = max_width.saturating_sub(cursor_space + 6).max(1);
        let mut start = if focused {
            self.cursor.saturating_sub(capacity / 2)
        } else {
            0
        };
        let mut end = (start + capacity).min(self.chars.len());
        start = end.saturating_sub(capacity);
        while UnicodeWidthStr::width(self.chars[start..end].iter().collect::<String>().as_str())
            > capacity
            && end > start
        {
            if self.cursor.saturating_sub(start) > end.saturating_sub(self.cursor) {
                start += 1;
            } else {
                end -= 1;
            }
        }
        let cursor = UnicodeWidthStr::width(
            self.chars[start..self.cursor.clamp(start, end)]
                .iter()
                .collect::<String>()
                .as_str(),
        ) + if start > 0 { 3 } else { 0 };
        (
            format!(
                "{}{}{}",
                if start > 0 { "..." } else { "" },
                self.chars[start..end].iter().collect::<String>(),
                if end < self.chars.len() { "..." } else { "" }
            ),
            u16::try_from(cursor).unwrap_or(u16::MAX),
        )
    }

    fn rendered_value(&self) -> String {
        self.value()
    }

    fn current_line_value(&self, max_width: usize) -> (String, u16) {
        let start = self.chars[..self.cursor]
            .iter()
            .rposition(|character| *character == '\n')
            .map_or(0, |position| position + 1);
        let end = self.chars[self.cursor..]
            .iter()
            .position(|character| *character == '\n')
            .map_or(self.chars.len(), |position| self.cursor + position);
        Self {
            chars: self.chars[start..end].to_vec(),
            cursor: self.cursor - start,
            vertical_column: None,
        }
        .display_value(true, max_width)
    }

    fn cursor_position(&self) -> (u16, u16) {
        let before = self.chars[..self.cursor].iter().collect::<String>();
        (
            u16::try_from(before.matches('\n').count()).unwrap_or(u16::MAX),
            u16::try_from(UnicodeWidthStr::width(
                before.rsplit('\n').next().unwrap_or_default(),
            ))
            .unwrap_or(u16::MAX),
        )
    }

    fn scroll_offset(&self, width: u16, height: u16) -> (u16, u16) {
        let before = self.chars[..self.cursor].iter().collect::<String>();
        let line = before.matches('\n').count();
        let column = UnicodeWidthStr::width(before.rsplit('\n').next().unwrap_or_default());
        (
            u16::try_from(line.saturating_sub(usize::from(height.saturating_sub(1))))
                .unwrap_or(u16::MAX),
            u16::try_from(column.saturating_sub(usize::from(width.saturating_sub(1))))
                .unwrap_or(u16::MAX),
        )
    }

    fn insert_char(&mut self, character: char) {
        self.vertical_column = None;
        self.chars.insert(self.cursor, character);
        self.cursor += 1;
    }

    fn insert_text(&mut self, text: &str) {
        for character in text.chars() {
            self.insert_char(character);
        }
    }

    fn handle_key(&mut self, key: KeyEvent, multiline: bool) {
        if multiline && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            self.move_vertical(key.code == KeyCode::Down);
            return;
        }
        self.vertical_column = None;
        match key.code {
            KeyCode::Char('a') if control_shortcut(key) => self.cursor = 0,
            KeyCode::Char('e') if control_shortcut(key) => {
                self.cursor = self.chars.len();
            }
            KeyCode::Char('u') if control_shortcut(key) => {
                self.chars.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('w') if control_shortcut(key) => {
                let mut start = self.cursor;
                while start > 0 && self.chars[start - 1].is_whitespace() {
                    start -= 1;
                }
                while start > 0 && !self.chars[start - 1].is_whitespace() {
                    start -= 1;
                }
                self.chars.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char(character) if text_entry_key(key) => {
                self.insert_char(character);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.chars.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.chars.len(),
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.chars.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.chars.len() => {
                self.chars.remove(self.cursor);
            }
            KeyCode::Enter if multiline => self.insert_char('\n'),
            _ => {}
        }
    }

    fn move_vertical(&mut self, down: bool) {
        let current_start = self.chars[..self.cursor]
            .iter()
            .rposition(|character| *character == '\n')
            .map_or(0, |position| position + 1);
        let column = *self.vertical_column.get_or_insert_with(|| {
            UnicodeWidthStr::width(
                self.chars[current_start..self.cursor]
                    .iter()
                    .collect::<String>()
                    .as_str(),
            )
        });
        let (target_start, target_end) = if down {
            let current_end = self.chars[self.cursor..]
                .iter()
                .position(|character| *character == '\n')
                .map_or(self.chars.len(), |position| self.cursor + position);
            if current_end == self.chars.len() {
                return;
            }
            let target_start = current_end + 1;
            let target_end = self.chars[target_start..]
                .iter()
                .position(|character| *character == '\n')
                .map_or(self.chars.len(), |position| target_start + position);
            (target_start, target_end)
        } else {
            if current_start == 0 {
                return;
            }
            let target_end = current_start - 1;
            let target_start = self.chars[..target_end]
                .iter()
                .rposition(|character| *character == '\n')
                .map_or(0, |position| position + 1);
            (target_start, target_end)
        };
        let mut width = 0;
        self.cursor = target_start;
        for (offset, character) in self.chars[target_start..target_end].iter().enumerate() {
            let next_width = width + UnicodeWidthChar::width(*character).unwrap_or(0);
            if next_width.abs_diff(column) <= width.abs_diff(column) {
                self.cursor = target_start + offset + 1;
                width = next_width;
            } else {
                break;
            }
        }
    }
}

fn edit_text_input(
    terminal: &mut TerminalSession,
    input: &mut TextInput,
    multiline: bool,
) -> TuiResult<()> {
    let editor = configured_editor()?;
    let suffix = if multiline { ".md" } else { ".txt" };
    let mut temporary = tempfile::Builder::new()
        .prefix("researchpocket-editor-")
        .suffix(suffix)
        .tempfile()?;
    temporary.write_all(input.value().as_bytes())?;
    temporary.flush()?;
    let path = temporary.into_temp_path();

    let mut command = Command::new(&editor[0]);
    command.args(&editor[1..]).arg(path.as_os_str());
    let status = terminal.run_child(&mut command)?;
    if !status.success() {
        return Err(io::Error::other(format!("editor exited with {status}")).into());
    }

    let edited = fs::read_to_string(&path)?;
    input.replace(normalize_editor_text(&edited, multiline));
    Ok(())
}

fn configured_editor() -> TuiResult<Vec<String>> {
    let configured = ["VISUAL", "EDITOR"]
        .into_iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.trim().is_empty()))
        .ok_or_else(|| io::Error::other("set VISUAL or EDITOR to use Ctrl+G"))?;
    parse_editor_command(&configured)
}

fn parse_editor_command(configured: &str) -> TuiResult<Vec<String>> {
    let editor = shell_words::split(configured).map_err(|error| {
        io::Error::other(format!("invalid VISUAL or EDITOR value: {error}"))
    })?;
    if editor.is_empty() {
        return Err(io::Error::other("VISUAL or EDITOR cannot be blank").into());
    }
    Ok(editor)
}

fn normalize_editor_text(value: &str, multiline: bool) -> String {
    if multiline {
        value.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        single_line(value)
    }
}

fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(Color::Black)),
        area,
    );
    if area.width < 40 || area.height < 12 {
        frame.render_widget(
            Paragraph::new(
                "ResearchPocket needs at least a 40 x 12 terminal. Resize, press Esc to close a dialog, or Ctrl+C to exit.",
            )
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(2),
        ])
        .split(area);
    render_header(frame, rows[0], app);
    render_body(frame, rows[1], app);
    render_footer(frame, rows[2], app);

    match &app.mode {
        Mode::Form(form) => render_form(frame, area, form, app.notice.as_ref()),
        Mode::DocumentForm(form) => {
            render_document_form(frame, area, form, app.notice.as_ref());
        }
        Mode::SyncSetup(form) => render_sync_setup(frame, area, form, app.notice.as_ref()),
        Mode::ConfirmDelete => {
            let (kind, name) = match app.workspace {
                Workspace::Library => (
                    "save",
                    app.selected_item()
                        .and_then(|item| item.title.as_deref())
                        .filter(|title| !title.is_empty()),
                ),
                Workspace::Documents => (
                    "document",
                    app.reader
                        .as_ref()
                        .map_or_else(
                            || app.selected_document().and_then(|d| d.title.as_deref()),
                            |reader| reader.title.as_deref(),
                        )
                        .filter(|title| !title.is_empty()),
                ),
            };
            render_confirmation(frame, area, kind, name);
        }
        Mode::ConfirmForceEnrich(target) => {
            render_force_enrichment_confirmation(frame, area, target);
        }
        Mode::Help => render_help(frame, area, app.workspace),
        Mode::Browse | Mode::Reader | Mode::Search(_) => {}
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let query = app.active_query();
    let search = if query.is_empty() {
        "no search".to_owned()
    } else {
        format!("search: {}", terminal_safe(query))
    };
    let filter = match app.workspace {
        Workspace::Library => format!(
            "{} | {}{}",
            search,
            app.lifecycle.label(),
            if app.favorite_only {
                " | favorites"
            } else {
                ""
            }
        ),
        Workspace::Documents => {
            format!("{} | {}", search, app.document_lifecycle.label())
        }
    };
    let title = Line::from(vec![
        Span::styled(
            "ResearchPocket",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            app.workspace.label(),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" Tab: {} ", app.workspace.other().label()),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(filter, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(
        Paragraph::new(title)
            .block(Block::default().borders(Borders::BOTTOM))
            .alignment(Alignment::Left),
        area,
    );
}

fn render_body(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    // An open document takes the whole body: prose deserves the width, and the
    // index it came from has nothing to add while it is being read.
    if matches!(app.mode, Mode::Reader)
        && let Some(reader) = &mut app.reader
    {
        render_reader(frame, area, reader);
        return;
    }
    let parts = if area.width < MIN_COMFORTABLE_WIDTH || area.height < MIN_COMFORTABLE_HEIGHT {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
            .split(area)
    };
    match app.workspace {
        Workspace::Library => {
            render_list(frame, parts[0], app);
            render_detail(frame, parts[1], app.selected_item(), app.detail_scroll);
        }
        Workspace::Documents => {
            render_document_list(frame, parts[0], app);
            render_document_detail(frame, parts[1], app.selected_document(), app.detail_scroll);
        }
    }
}

fn render_document_list(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let items = app
        .documents
        .iter()
        .map(|document| {
            let title = document
                .title
                .as_deref()
                .filter(|title| !title.is_empty())
                .unwrap_or("Untitled document");
            let state = if document.lifecycle_state == LifecycleState::Deleted {
                " [deleted]"
            } else {
                ""
            };
            let mut meta = vec![format_bytes(document.byte_length)];
            if document.todo_total > 0 {
                meta.push(format!(
                    "{}/{} done",
                    document.todo_done, document.todo_total
                ));
            }
            if !document.tags.is_empty() {
                meta.push(document.tags.join(", "));
            }
            ListItem::new(vec![
                Line::from(format!("  {}{state}", terminal_safe(title))),
                Line::from(Span::styled(
                    format!("  {}", terminal_safe(&meta.join(" | "))),
                    Style::default().fg(Color::DarkGray),
                )),
            ])
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(
            Block::default()
                .title(format!(" Documents ({}) ", app.documents.len()))
                .borders(Borders::ALL),
        )
        .highlight_symbol("> ")
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, area, &mut app.document_list_state);
}

/// The index detail pane. Metadata only: a body is read when a document is
/// opened, never to fill a preview.
fn render_document_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    document: Option<&ZenDocumentSummary>,
    scroll: u16,
) {
    let text = document.map_or_else(
        || Text::from("No documents match the current view. Press a to write one."),
        |document| {
            let mut lines = vec![
                detail_line(
                    "Title",
                    document.title.as_deref().unwrap_or("Untitled document"),
                ),
                detail_line("Created", format_timestamp(document.created_at)),
                detail_line(
                    "State",
                    match document.lifecycle_state {
                        LifecycleState::Active => "active",
                        LifecycleState::Deleted => "deleted",
                    },
                ),
                detail_line("Size", format_bytes(document.byte_length)),
                detail_line(
                    "Tasks",
                    if document.todo_total == 0 {
                        "-".to_owned()
                    } else {
                        format!("{} of {} done", document.todo_done, document.todo_total)
                    },
                ),
                detail_line(
                    "Tags",
                    if document.tags.is_empty() {
                        "-".to_owned()
                    } else {
                        document.tags.join(", ")
                    },
                ),
                Line::default(),
                detail_line("ID", &document.document_id),
                Line::default(),
                Line::styled(
                    "Enter reads it | e edits it | x deletes it",
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            lines.push(Line::default());
            lines.push(Line::styled(
                "The body is read only when the document is opened.",
                Style::default().fg(Color::DarkGray),
            ));
            Text::from(lines)
        },
    );
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default().title(" Document ").borders(Borders::ALL))
            .scroll((scroll, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Renders the open document and keeps the cursor line in view.
fn render_reader(frame: &mut Frame<'_>, area: Rect, reader: &mut Reader) {
    let title = reader
        .title
        .clone()
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "Untitled document".to_owned());
    let block = Block::default()
        .title(format!(
            " {}{} ",
            terminal_snippet(&title, usize::from(area.width.saturating_sub(8))),
            if reader.deleted { " [deleted]" } else { "" }
        ))
        .borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let text_area = parts[0];

    // A prose column that stops widening, with the gutter that marks the
    // cursor line taken out of it.
    let column = text_area.width.min(READING_COLUMN);
    let margin = (text_area.width - column) / 2;
    let width = column.saturating_sub(GUTTER_WIDTH).max(1);
    let height = usize::from(text_area.height).max(1);

    let cursor = reader.cursor;
    let (first, last) = {
        let rows = reader.rows(width);
        let first = rows
            .iter()
            .position(|row| row.source == cursor)
            .unwrap_or(0);
        let last = rows
            .iter()
            .rposition(|row| row.source == cursor)
            .unwrap_or(first);
        (first, last)
    };
    let total = reader.row_count();
    if first < reader.scroll {
        reader.scroll = first;
    } else if last >= reader.scroll + height {
        reader.scroll = last + 1 - height;
    }
    reader.scroll = reader.scroll.min(total.saturating_sub(height));
    let scroll = reader.scroll;
    let lines = reader
        .rows(width)
        .iter()
        .skip(scroll)
        .take(height)
        .map(|row| {
            let mut spans = vec![if row.source == cursor {
                Span::styled("▌ ", Style::default().fg(Color::Cyan))
            } else {
                Span::raw("  ")
            }];
            spans.extend(row.line.spans.iter().cloned());
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines),
        Rect {
            x: text_area.x + margin,
            width: column,
            ..text_area
        },
    );

    let mut status = vec![format!("line {} of {}", cursor + 1, reader.line_count())];
    status.push(format_bytes(reader.body().len()));
    if !reader.tags.is_empty() {
        status.push(reader.tags.join(", "));
    }
    status.push("Space toggles a task | e edits | Ctrl+G editor | Esc closes".to_owned());
    frame.render_widget(
        Paragraph::new(Line::styled(
            terminal_snippet(&status.join(" | "), usize::from(parts[1].width)),
            Style::default().fg(Color::DarkGray),
        )),
        parts[1],
    );
}

fn render_list(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let items = app
        .items
        .iter()
        .map(|item| {
            let title = item
                .title
                .as_deref()
                .filter(|title| !title.is_empty())
                .unwrap_or("Untitled");
            let marker = if item.favorite { "* " } else { "  " };
            let state = if item.state == "deleted" {
                " [deleted]"
            } else {
                ""
            };
            ListItem::new(vec![
                Line::from(format!("{marker}{}{state}", terminal_safe(title))),
                Line::from(Span::styled(
                    terminal_safe(&item.url),
                    Style::default().fg(Color::DarkGray),
                )),
            ])
        })
        .collect::<Vec<_>>();
    let title = format!(" Saves ({}) ", app.items.len());
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_symbol("> ")
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_detail(frame: &mut Frame<'_>, area: Rect, item: Option<&StoredItem>, scroll: u16) {
    let text = item.map_or_else(
        || Text::from("No saves match the current view."),
        |item| {
            let mut lines = vec![
                detail_line("Title", item.title.as_deref().unwrap_or("Untitled")),
                detail_line("URL", &item.url),
                detail_line("Saved", &item.saved_at),
                detail_line("State", &item.state),
                detail_line("Favorite", if item.favorite { "yes" } else { "no" }),
                detail_line(
                    "Tags",
                    if item.tags.is_empty() {
                        "-".to_owned()
                    } else {
                        item.tags.join(", ")
                    },
                ),
            ];
            if let Some(excerpt) = &item.excerpt {
                lines.push(Line::default());
                lines.push(heading("Excerpt"));
                lines.extend(excerpt.lines().map(|line| Line::from(terminal_safe(line))));
            }
            if let Some(note) = &item.note {
                lines.push(Line::default());
                lines.push(heading("Private note"));
                lines.extend(note.lines().map(|line| Line::from(terminal_safe(line))));
            }
            lines.push(Line::default());
            lines.push(detail_line("ID", &item.id));
            Text::from(lines)
        },
    );
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default().title(" Details ").borders(Borders::ALL))
            .scroll((scroll, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let compact = area.width < 78;
    let pending = app.status.pending_updates + app.aggregates_pending;
    let mut status = if compact {
        format!(
            "{} active | {} deleted | {pending} pending",
            app.status.active_items, app.status.deleted_items
        )
    } else {
        format!(
            "{} active | {} deleted | {pending} pending | sync: {}",
            app.status.active_items, app.status.deleted_items, app.status.sync_state
        )
    };
    if let Some(operation) = &app.operation {
        status.push_str(" | ");
        status.push_str(operation.label);
    }
    let message = match &app.mode {
        Mode::Search(input) => {
            let (value, _) =
                input.display_value(true, usize::from(area.width.saturating_sub(28)));
            Line::from(vec![
                Span::styled(
                    match app.workspace {
                        Workspace::Library => "Search: ",
                        Workspace::Documents => "Filter: ",
                    },
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(terminal_safe(&value)),
                Span::styled(
                    app.notice.as_ref().map_or(
                        "  Enter apply | Ctrl+G editor | Esc cancel".to_owned(),
                        |notice| format!("  error: {}", terminal_safe(&notice.text)),
                    ),
                    Style::default().fg(if app.notice.is_some() {
                        Color::Red
                    } else {
                        Color::DarkGray
                    }),
                ),
            ])
        }
        _ => {
            if let Some(notice) = &app.notice {
                Line::styled(
                    terminal_safe(&notice.text),
                    Style::default().fg(if notice.error {
                        Color::Red
                    } else {
                        Color::Green
                    }),
                )
            } else {
                Line::styled(
                    match (app.workspace, &app.mode, compact) {
                        (_, Mode::Reader, true) => "Space task | e edit | Esc close",
                        (_, Mode::Reader, false) => {
                            "Space task | e edit | Ctrl+G editor | x delete | Esc close | ? help"
                        }
                        (Workspace::Library, _, true) => {
                            "a add | E enrich | s sync | Tab documents | ? help"
                        }
                        (Workspace::Library, _, false) => {
                            "a add | e edit | E enrich | Ctrl+E replace | s sync | Tab documents | ? help | q quit"
                        }
                        (Workspace::Documents, _, true) => {
                            "a new | Enter read | s sync | Tab saves | ? help"
                        }
                        (Workspace::Documents, _, false) => {
                            "a new | Enter read | e edit | x delete | s sync | Tab saves | ? help | q quit"
                        }
                    },
                    Style::default().fg(Color::DarkGray),
                )
            }
        }
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(status, Style::default().fg(Color::DarkGray)),
            message,
        ]),
        area,
    );
    if let Mode::Search(input) = &app.mode {
        let (_, cursor) = input.display_value(true, usize::from(area.width.saturating_sub(28)));
        frame.set_cursor_position((
            area.x
                .saturating_add(UnicodeWidthStr::width("Search: ") as u16)
                .saturating_add(cursor),
            area.y.saturating_add(1),
        ));
    }
}

fn render_form(frame: &mut Frame<'_>, area: Rect, form: &ItemForm, notice: Option<&Notice>) {
    if area.width < 50 || area.height < 30 {
        render_compact_form(frame, area, form, notice);
        return;
    }
    let popup = centered_rect(area, 90, if form.can_enrich() { 36 } else { 34 });
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(format!(" {} ", form.title()))
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut constraints = vec![Constraint::Length(3); form.fields.len()];
    constraints[2] = Constraint::Fill(2);
    constraints[3] = Constraint::Fill(1);
    constraints.push(Constraint::Length(2));
    if form.can_enrich() {
        constraints.push(Constraint::Length(2));
    }
    constraints.push(Constraint::Length(2));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    render_form_fields(frame, &rows, &form.fields, form.active);
    let favorite_style = if form.active == form.fields.len() {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{}{} Favorite",
            if form.active == form.fields.len() {
                "> "
            } else {
                "  "
            },
            if form.favorite { "[x]" } else { "[ ]" }
        ))
        .style(favorite_style),
        rows[form.fields.len()],
    );
    let instructions_index = if form.can_enrich() {
        let enrich_index = form.fields.len() + 1;
        let enrich_style = if form.active == enrich_index {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(format!(
                "{}{} Enrich after save",
                if form.active == enrich_index {
                    "> "
                } else {
                    "  "
                },
                if form.enrich { "[x]" } else { "[ ]" }
            ))
            .style(enrich_style),
            rows[enrich_index],
        );
        enrich_index + 1
    } else {
        form.fields.len() + 1
    };
    let instructions = notice.map_or_else(
        || {
            vec![
                Line::from("Tab/Shift+Tab fields | Up/Down multiline | Ctrl+N newline"),
                Line::from("Ctrl+W word | Ctrl+G editor | Ctrl+S save | Esc cancel"),
            ]
        },
        |notice| {
            vec![Line::styled(
                terminal_safe(&notice.text),
                Style::default().fg(if notice.error {
                    Color::Red
                } else {
                    Color::Green
                }),
            )]
        },
    );
    frame.render_widget(
        Paragraph::new(instructions).style(Style::default().fg(Color::DarkGray)),
        rows[instructions_index],
    );
}

/// Draws one bordered input per field and parks the terminal cursor in the
/// focused one.
fn render_form_fields(
    frame: &mut Frame<'_>,
    rows: &[Rect],
    fields: &[FormField],
    active_field: usize,
) {
    for (index, field) in fields.iter().enumerate() {
        let active = index == active_field;
        let input_area = rows[index];
        let input_width = input_area.width.saturating_sub(2).max(1);
        let input_height = input_area.height.saturating_sub(2).max(1);
        let rendered = field.input.rendered_value();
        let rendered = if field.multiline {
            terminal_safe_multiline(&rendered)
        } else {
            terminal_safe(&rendered)
        };
        frame.render_widget(
            Paragraph::new(rendered)
                .block(
                    Block::default()
                        .title(format!(" {} ", field.label))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(if active {
                            Color::Cyan
                        } else {
                            Color::DarkGray
                        })),
                )
                .scroll(if active {
                    field.input.scroll_offset(input_width, input_height)
                } else {
                    (0, 0)
                }),
            input_area,
        );
        if active {
            let scroll = field.input.scroll_offset(input_width, input_height);
            let cursor = field.input.cursor_position();
            frame.set_cursor_position((
                input_area
                    .x
                    .saturating_add(1)
                    .saturating_add(cursor.1.saturating_sub(scroll.1)),
                input_area
                    .y
                    .saturating_add(1)
                    .saturating_add(cursor.0.saturating_sub(scroll.0)),
            ));
        }
    }
}

/// The document form: a title, a body that takes every spare row, and tags.
fn render_document_form(
    frame: &mut Frame<'_>,
    area: Rect,
    form: &DocumentForm,
    notice: Option<&Notice>,
) {
    if area.width < 50 || area.height < 18 {
        render_compact_document_form(frame, area, form, notice);
        return;
    }
    let popup = centered_rect(area, 96, 40);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(format!(" {} ", form.title()))
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Fill(1),
            Constraint::Length(3),
            Constraint::Length(2),
        ])
        .split(inner);
    render_form_fields(frame, &rows, &form.fields, form.active);
    let instructions = notice.map_or_else(
        || {
            vec![
                Line::from(format!(
                    "{} of {} | Tab/Shift+Tab fields | Up/Down body lines | Ctrl+N newline",
                    format_bytes(form.body_bytes()),
                    format_bytes(MAX_ZEN_BODY_BYTES)
                )),
                Line::from(
                    "Mention a save as [label](research:item/<uuid>) | Ctrl+G editor | Ctrl+S save | Esc cancel",
                ),
            ]
        },
        |notice| {
            vec![Line::styled(
                terminal_safe(&notice.text),
                Style::default().fg(if notice.error {
                    Color::Red
                } else {
                    Color::Green
                }),
            )]
        },
    );
    frame.render_widget(
        Paragraph::new(instructions).style(Style::default().fg(Color::DarkGray)),
        rows[3],
    );
}

fn render_compact_document_form(
    frame: &mut Frame<'_>,
    area: Rect,
    form: &DocumentForm,
    notice: Option<&Notice>,
) {
    let popup = centered_rect(area, 48, 10);
    frame.render_widget(Clear, popup);
    let field = &form.fields[form.active];
    let (value, cursor) = field
        .input
        .current_line_value(usize::from(popup.width.saturating_sub(4)));
    let hint = notice.map_or("Ctrl+G editor | Tab fields | Ctrl+S save", |notice| {
        notice.text.as_str()
    });
    frame.render_widget(
        Paragraph::new(format!(
            "{}\n\n{}\n\n{}\n{}",
            field.label,
            terminal_safe(&value),
            format_bytes(form.body_bytes()),
            terminal_safe(hint)
        ))
        .block(
            Block::default()
                .title(format!(" {} ", form.title()))
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false }),
        popup,
    );
    frame.set_cursor_position((
        popup.x.saturating_add(1).saturating_add(cursor),
        popup.y.saturating_add(3),
    ));
}

fn render_compact_form(
    frame: &mut Frame<'_>,
    area: Rect,
    form: &ItemForm,
    notice: Option<&Notice>,
) {
    let popup = centered_rect(area, 48, 10);
    frame.render_widget(Clear, popup);
    let mut cursor = None;
    let text = if let Some(field) = form.fields.get(form.active) {
        let (value, column) = field
            .input
            .current_line_value(usize::from(popup.width.saturating_sub(4)));
        cursor = Some(column);
        let hint = notice.map_or("Ctrl+G editor | Tab fields | Ctrl+S save", |notice| {
            notice.text.as_str()
        });
        format!(
            "{}\n\n{}\n\nFavorite: {} | Enrich: {}\n{}",
            field.label,
            terminal_safe(&value),
            if form.favorite { "yes" } else { "no" },
            if form.can_enrich() {
                if form.enrich { "yes" } else { "no" }
            } else {
                "n/a"
            },
            terminal_safe(hint)
        )
    } else if form.active == form.fields.len() {
        format!(
            "Favorite: {}\n\nSpace toggles\nTab fields | Ctrl+S save | Esc cancel",
            if form.favorite { "yes" } else { "no" }
        )
    } else {
        format!(
            "Enrich after save: {}\n\nUses the configured provider\nSpace toggles | Ctrl+S save | Esc cancel",
            if form.enrich { "yes" } else { "no" }
        )
    };
    frame.render_widget(
        Paragraph::new(text)
            .block(
                Block::default()
                    .title(format!(" {} ", form.title()))
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
    if let Some(column) = cursor {
        frame.set_cursor_position((
            popup.x.saturating_add(1).saturating_add(column),
            popup.y.saturating_add(3),
        ));
    }
}

fn render_sync_setup(
    frame: &mut Frame<'_>,
    area: Rect,
    form: &SyncForm,
    notice: Option<&Notice>,
) {
    if area.width < 50 || area.height < 15 {
        render_compact_sync_setup(frame, area, form, notice);
        return;
    }
    let popup = centered_rect(area, 74, 15);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(" Connect GitHub sync ")
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(2),
        ])
        .split(inner);
    for (index, field) in form.fields.iter().enumerate() {
        let active = index == form.active;
        let (value, cursor) = field
            .input
            .display_value(active, usize::from(rows[index].width.saturating_sub(4)));
        frame.render_widget(
            Paragraph::new(terminal_safe(&value)).block(
                Block::default()
                    .title(format!(" {} ", field.label))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(if active {
                        Color::Cyan
                    } else {
                        Color::DarkGray
                    })),
            ),
            rows[index],
        );
        if active {
            frame.set_cursor_position((
                rows[index].x.saturating_add(1).saturating_add(cursor),
                rows[index].y.saturating_add(1),
            ));
        }
    }
    frame.render_widget(
        Paragraph::new(
            "Uses RESEARCHPOCKET_GITHUB_TOKEN or GH_TOKEN from this process.\nCtrl+G editor | Tab fields | Ctrl+S connect and sync",
        )
        .style(Style::default().fg(Color::DarkGray)),
        rows[2],
    );
    if let Some(notice) = notice {
        frame.render_widget(
            Paragraph::new(terminal_safe(&notice.text))
                .style(Style::default().fg(if notice.error {
                    Color::Red
                } else {
                    Color::Green
                }))
                .wrap(Wrap { trim: false }),
            rows[3],
        );
    }
}

fn render_compact_sync_setup(
    frame: &mut Frame<'_>,
    area: Rect,
    form: &SyncForm,
    notice: Option<&Notice>,
) {
    let popup = centered_rect(area, 48, 10);
    frame.render_widget(Clear, popup);
    let field = &form.fields[form.active];
    let (value, cursor) = field
        .input
        .current_line_value(usize::from(popup.width.saturating_sub(4)));
    let mut lines = vec![
        Line::from(field.label),
        Line::default(),
        Line::from(terminal_safe(&value)),
        Line::default(),
        Line::styled(
            "Ctrl+G editor | Tab fields | Ctrl+S connect",
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if let Some(notice) = notice {
        lines.push(Line::styled(
            terminal_safe(&notice.text),
            Style::default().fg(if notice.error {
                Color::Red
            } else {
                Color::Green
            }),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" Connect GitHub sync ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
    frame.set_cursor_position((
        popup.x.saturating_add(1).saturating_add(cursor),
        popup.y.saturating_add(3),
    ));
}

fn render_confirmation(frame: &mut Frame<'_>, area: Rect, kind: &str, name: Option<&str>) {
    let popup = centered_rect(area, 58, 7);
    frame.render_widget(Clear, popup);
    let title = name.unwrap_or(kind);
    frame.render_widget(
        Paragraph::new(format!(
            "Delete {}?\n\nThis is recoverable. Press y/Enter to confirm or n/Esc to cancel.",
            terminal_snippet(title, usize::from(popup.width.saturating_sub(12)))
        ))
        .block(
            Block::default()
                .title(" Confirm delete ")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false }),
        popup,
    );
}

fn render_force_enrichment_confirmation(
    frame: &mut Frame<'_>,
    area: Rect,
    target: &ForceEnrichmentConfirmation,
) {
    let popup = centered_rect(area, 68, 9);
    frame.render_widget(Clear, popup);
    let title = target
        .title
        .as_deref()
        .filter(|title| !title.is_empty())
        .unwrap_or("this save");
    frame.render_widget(
        Paragraph::new(format!(
            "Replace the excerpt for {}?\n\nThis re-fetches the page with the configured provider. It applies only if the excerpt does not change while the request runs.\n\nPress y/Enter to confirm or n/Esc to cancel.",
            terminal_snippet(title, usize::from(popup.width.saturating_sub(18)))
        ))
        .block(
            Block::default()
                .title(" Confirm excerpt replacement ")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false }),
        popup,
    );
}

fn render_help(frame: &mut Frame<'_>, area: Rect, workspace: Workspace) {
    if area.width < 78 || area.height < 25 {
        let popup = centered_rect(area, 48, 12);
        frame.render_widget(Clear, popup);
        let help: &[&str] = match workspace {
            Workspace::Library => &[
                "j/k or arrows move | g/G first/last",
                "a/e add/edit | / search | Ctrl+G editor",
                "E enrich | Ctrl+E replace excerpt",
                "Space favorite | x delete | r restore",
                "f favorites | d views | R refresh",
                "Tab documents | s connect/sync",
                "Esc cancel/clear | ? close help",
                "q in a list or Ctrl+C exits",
            ],
            Workspace::Documents => &[
                "j/k or arrows move | g/G first/last",
                "Enter read | a new | e edit | / filter",
                "x delete | r restore | d views",
                "In a document: Space toggles a task",
                "Ctrl+G edits the body in your editor",
                "Tab saves | s connect/sync | R refresh",
                "Esc closes | ? close help",
                "q in a list or Ctrl+C exits",
            ],
        };
        frame.render_widget(
            Paragraph::new(help.join("\n"))
                .block(
                    Block::default()
                        .title(" Keyboard help ")
                        .borders(Borders::ALL),
                )
                .wrap(Wrap { trim: false }),
            popup,
        );
        return;
    }
    let popup = centered_rect(area, 76, 27);
    frame.render_widget(Clear, popup);
    let mut help = vec![
        "Navigation",
        "  j/k or arrows   move selection     g/G or Home/End   first/last",
        "  PgUp/PgDn       move ten            R                 refresh",
        "  Ctrl+U/Ctrl+D   scroll              Tab               other workspace",
        "",
    ];
    match workspace {
        Workspace::Library => help.extend([
            "Saves",
            "  a add           e/Enter edit        Space toggle favorite",
            "  x delete        r restore           / search",
            "  E enrich        Ctrl+E replace       s connect/sync",
            "  f favorites     d lifecycle view    Esc clear search",
            "",
            "Enrichment uses the configured local provider. Sync uses a PAT from",
            "RESEARCHPOCKET_GITHUB_TOKEN or GH_TOKEN and never persists it.",
        ]),
        Workspace::Documents => help.extend([
            "Documents",
            "  a new           e edit              Enter/o read",
            "  x delete        r restore           / filter title and tags",
            "  d lifecycle view                    s connect/sync",
            "",
            "Reading a document",
            "  j/k move the cursor line            Space or t toggle its task",
            "  e edit in a form                    Ctrl+G edit the body in $EDITOR",
            "  R reload            x delete        Esc or q close",
            "",
            "Bodies are read only when a document is opened. A mention written as",
            "[label](research:item/<uuid>) resolves against this library as you read.",
        ]),
    }
    help.extend([
        "",
        "Forms",
        "  Tab/Shift+Tab fields                Space toggles options",
        "  Up/Down multiline fields            Ctrl+N inserts newline",
        "  Ctrl+W deletes previous word        Ctrl+G opens terminal editor",
        "  Ctrl+S commits mutation             Esc cancels",
        "",
        "Press ?, Enter, or Esc to close help.",
    ]);
    frame.render_widget(
        Paragraph::new(help.join("\n"))
            .block(
                Block::default()
                    .title(" Keyboard help ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn centered_rect(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = max_width.min(area.width.saturating_sub(2)).max(1);
    let height = max_height.min(area.height.saturating_sub(2)).max(1);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn heading(text: &'static str) -> Line<'static> {
    Line::styled(
        text,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}

fn detail_line<'a>(label: &'static str, value: impl Into<String>) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(terminal_safe(&value.into())),
    ])
}

fn enrichment_notice(action: &str, outcome: &v2::EnrichmentAttemptOutcome) -> Notice {
    match outcome.status() {
        StoreEnrichmentStatus::Succeeded => {
            let fields = outcome.applied_fields().join(", ");
            Notice::info(format!("{action}; enriched {fields}"))
        }
        StoreEnrichmentStatus::Skipped => {
            Notice::info(format!("{action}; no eligible metadata was missing"))
        }
        StoreEnrichmentStatus::Retry => Notice::info(format!(
            "{action}; enrichment queued for retry ({})",
            outcome.last_error_kind().unwrap_or("provider_error")
        )),
        StoreEnrichmentStatus::Failed => Notice::error(format!(
            "{action}; enrichment retries exhausted ({})",
            outcome.last_error_kind().unwrap_or("provider_error")
        )),
        StoreEnrichmentStatus::InProgress => {
            Notice::info(format!("{action}; enrichment is already in progress"))
        }
        StoreEnrichmentStatus::Pending => {
            Notice::info(format!("{action}; enrichment remains queued"))
        }
    }
}

fn optional_text_change(original: Option<&str>, value: &str) -> Option<OptionalTextUpdate> {
    if original.unwrap_or_default() == value {
        None
    } else if value.is_empty() {
        Some(OptionalTextUpdate::Clear)
    } else {
        Some(OptionalTextUpdate::Set(value.to_owned()))
    }
}

fn format_tags(tags: &[String]) -> String {
    Value::Array(tags.iter().cloned().map(Value::String).collect()).to_string()
}

fn parse_tags(value: &str) -> Result<Vec<String>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let tags = if value.starts_with('[') {
        serde_json::from_str::<Vec<String>>(value).map_err(|_| {
            "tags JSON must be an array of strings, such as [\"reading\", \"rust\"]".to_owned()
        })?
    } else {
        value
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect()
    };
    if tags.iter().any(|tag| tag.trim().is_empty()) {
        return Err("tags cannot be empty or whitespace-only".to_owned());
    }
    Ok(tags
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn single_line(value: &str) -> String {
    value.replace("\r\n", " ").replace(['\r', '\n'], " ")
}

fn terminal_safe(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

fn terminal_snippet(value: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let value = terminal_safe(value);
    if UnicodeWidthStr::width(value.as_str()) <= max_width {
        return value;
    }
    let mut snippet = String::new();
    for character in value.chars() {
        snippet.push(character);
        if UnicodeWidthStr::width(snippet.as_str()) >= max_width.saturating_sub(1) {
            while UnicodeWidthStr::width(snippet.as_str()) > max_width.saturating_sub(1) {
                snippet.pop();
            }
            break;
        }
    }
    snippet.push('…');
    snippet
}

fn terminal_safe_multiline(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character == '\n' {
                '\n'
            } else if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

fn control_shortcut(key: KeyEvent) -> bool {
    key.modifiers == KeyModifiers::CONTROL
}

fn command_key(key: KeyEvent) -> bool {
    !key.modifiers.intersects(
        KeyModifiers::CONTROL
            | KeyModifiers::ALT
            | KeyModifiers::SUPER
            | KeyModifiers::HYPER
            | KeyModifiers::META,
    )
}

fn text_entry_key(key: KeyEvent) -> bool {
    command_key(key)
        || key
            .modifiers
            .contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && !key
                .modifiers
                .intersects(KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn control_e_latches_only_the_browse_replacement_action() {
        let directory = tempfile::tempdir().expect("temporary library");
        let store = V2Store::init(directory.path())
            .await
            .expect("initialize library");
        let mut app = App::load(&store).await.expect("load TUI state");
        let key = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);

        app.mode = Mode::Form(Box::new(ItemForm::create()));
        assert!(app.accept_key_press(key));
        assert!(app.accept_key_press(key));

        app.mode = Mode::Browse;
        assert!(app.accept_key_press(key));
        assert!(!app.accept_key_press(key));
    }

    /// The workspace is exercised the way it is used: index, open, edit.
    #[tokio::test]
    async fn the_documents_workspace_reads_edits_and_toggles_one_document() {
        let directory = tempfile::tempdir().expect("temporary library");
        let store = V2Store::init(directory.path())
            .await
            .expect("initialize library");
        let item = store
            .create_item(CreateItemRequest {
                url: "https://example.com/paper".to_owned(),
                title: Some("The paper".to_owned()),
                excerpt: None,
                favorite: false,
                language: None,
                saved_at: None,
                note: String::new(),
                tags: Vec::new(),
            })
            .await
            .expect("save an item to mention");
        let document = store
            .create_zen_document(CreateZenDocumentRequest {
                title: Some("Today".to_owned()),
                body: format!("- [ ] Read [it](research:item/{})\n", item.id),
                tags: vec!["reading".to_owned()],
            })
            .await
            .expect("create document");

        let mut app = App::load(&store).await.expect("load TUI state");
        assert_eq!(app.documents.len(), 1, "the index loads beside the library");
        assert_eq!(app.workspace, Workspace::Library);

        app.handle_browse_key(
            &store,
            directory.path(),
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        )
        .await;
        assert_eq!(app.workspace, Workspace::Documents);

        app.open_selected_document(&store).await;
        assert!(matches!(app.mode, Mode::Reader));
        let reader = app.reader.as_ref().expect("open document");
        assert_eq!(reader.document_id, document.document_id);
        assert!(reader.body().starts_with("- [ ] Read"));

        app.toggle_open_todo(&store).await;
        let stored = store
            .zen_document(&document.document_id)
            .await
            .expect("read back");
        assert!(
            stored.body.starts_with("- [x] Read"),
            "the toggle reached the store: {}",
            stored.body
        );
        assert_eq!(
            (app.documents[0].todo_done, app.documents[0].todo_total),
            (1, 1),
            "the index follows the mutation"
        );

        // A buffer opened before a concurrent edit must not undo it.
        let form = DocumentForm::from_reader(app.reader.as_ref().expect("open document"));
        store
            .edit_zen_document(EditZenDocumentRequest {
                document_id: document.document_id.clone(),
                body: Some("elsewhere\n".to_owned()),
                ..EditZenDocumentRequest::default()
            })
            .await
            .expect("concurrent edit");
        app.mode = Mode::DocumentForm(Box::new(form));
        if let Mode::DocumentForm(form) = &mut app.mode {
            form.active = 1;
            form.fields[1].input.insert_text("stale");
        }
        app.submit_document_form(&store).await;
        assert!(
            app.notice.as_ref().is_some_and(|notice| notice.error),
            "a stale body replacement is refused"
        );
        assert_eq!(
            store
                .zen_document(&document.document_id)
                .await
                .expect("read back")
                .body,
            "elsewhere\n"
        );
    }

    #[tokio::test]
    async fn deleted_documents_are_only_listed_in_a_lifecycle_view_that_asks() {
        let directory = tempfile::tempdir().expect("temporary library");
        let store = V2Store::init(directory.path())
            .await
            .expect("initialize library");
        let document = store
            .create_zen_document(CreateZenDocumentRequest {
                title: Some("Draft".to_owned()),
                ..CreateZenDocumentRequest::default()
            })
            .await
            .expect("create document");

        let mut app = App::load(&store).await.expect("load TUI state");
        app.workspace = Workspace::Documents;
        app.mode = Mode::ConfirmDelete;
        app.delete_selected_document(&store).await;
        assert!(app.documents.is_empty(), "the active view hides it");

        app.document_lifecycle = LifecycleFilter::Deleted;
        app.refresh_documents(&store, None)
            .await
            .expect("refresh index");
        assert_eq!(app.documents.len(), 1);

        app.restore_selected_document(&store).await;
        app.document_lifecycle = LifecycleFilter::Active;
        app.refresh_documents(&store, Some(&document.document_id))
            .await
            .expect("refresh index");
        assert_eq!(app.documents.len(), 1);
    }

    #[tokio::test]
    async fn the_document_filter_matches_metadata_without_reading_a_body() {
        let directory = tempfile::tempdir().expect("temporary library");
        let store = V2Store::init(directory.path())
            .await
            .expect("initialize library");
        for (title, tag, body) in [
            ("Reading list", "reading", "needle in the body"),
            ("Grocery run", "errands", "nothing"),
        ] {
            store
                .create_zen_document(CreateZenDocumentRequest {
                    title: Some(title.to_owned()),
                    body: body.to_owned(),
                    tags: vec![tag.to_owned()],
                })
                .await
                .expect("create document");
        }

        let mut app = App::load(&store).await.expect("load TUI state");
        app.workspace = Workspace::Documents;
        app.document_query = "GROCERY".to_owned();
        app.refresh_documents(&store, None).await.expect("filter");
        assert_eq!(app.documents.len(), 1);

        app.document_query = "errands".to_owned();
        app.refresh_documents(&store, None).await.expect("filter");
        assert_eq!(app.documents.len(), 1, "tags match too");

        app.document_query = "needle".to_owned();
        app.refresh_documents(&store, None).await.expect("filter");
        assert!(
            app.documents.is_empty(),
            "filtering never becomes a reason to load bodies"
        );
    }

    #[test]
    fn multiline_fields_preserve_text_and_navigate_vertically() {
        let mut input = TextInput::new("abcd\nx\nwxyz".to_owned());
        input.cursor = 3;

        input.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), true);
        assert_eq!(input.cursor, 6);
        input.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), true);
        assert_eq!(input.cursor, 10);
        input.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true);
        assert_eq!(input.cursor, 6);
        input.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true);
        assert_eq!(input.cursor, 3);

        let mut wide_input = TextInput::new("ab界d\n12345".to_owned());
        wide_input.cursor = 3;
        wide_input.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), true);
        assert_eq!(wide_input.cursor, 9);
        wide_input.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true);
        assert_eq!(wide_input.cursor, 3);
        input.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true);
        assert_eq!(input.cursor, 3);

        let mut form = ItemForm::create();
        form.active = 2;
        form.insert_text("# Heading\n\nFull Markdown".to_owned());
        assert_eq!(form.fields[2].input.value(), "# Heading\n\nFull Markdown");
        form.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(form.active, 2);
        form.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(form.active, 3);
    }

    #[test]
    fn control_w_deletes_the_previous_word_without_moving_trailing_text() {
        let control_w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        let mut input = TextInput::new("first  second\nthird tail".to_owned());
        input.cursor = 20;

        input.handle_key(control_w, true);
        assert_eq!(input.value(), "first  second\ntail");
        assert_eq!(input.cursor, 14);

        input.handle_key(control_w, true);
        assert_eq!(input.value(), "first  tail");
        assert_eq!(input.cursor, 7);

        input.cursor = 0;
        input.handle_key(control_w, true);
        assert_eq!(input.value(), "first  tail");
    }

    #[test]
    fn text_input_reports_a_real_cursor_without_rendering_a_marker() {
        let mut input = TextInput::new("alpha beta".to_owned());
        input.cursor = 5;

        assert_eq!(input.display_value(true, 20), ("alpha beta".to_owned(), 5));
        assert_eq!(input.rendered_value(), "alpha beta");
        assert_eq!(input.cursor_position(), (0, 5));
    }

    #[test]
    fn editor_configuration_supports_arguments_without_shell_evaluation() {
        assert_eq!(
            parse_editor_command(r#""editor with spaces" --wait "profile name""#).unwrap(),
            ["editor with spaces", "--wait", "profile name"]
        );
        assert!(parse_editor_command(r#""unterminated"#).is_err());
    }

    #[test]
    fn editor_text_respects_the_field_line_policy() {
        assert_eq!(
            normalize_editor_text("first\r\nsecond\rthird", true),
            "first\nsecond\nthird"
        );
        assert_eq!(
            normalize_editor_text("first\r\nsecond\rthird", false),
            "first second third"
        );

        let mut input = TextInput::new("old".to_owned());
        input.replace("new\nvalue".to_owned());
        assert_eq!(input.value(), "new\nvalue");
        assert_eq!(input.cursor, input.chars.len());
    }
}
