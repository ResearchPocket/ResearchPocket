use std::error::Error;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use directories::ProjectDirs;
use research_store::{
    CreateItemRequest, CreateZenDocumentRequest, EditItemRequest, EditZenDocumentRequest,
    EnrichmentClaim, EnrichmentJob, EnrichmentProvider, EnrichmentQueueCounts,
    EnrichmentStatus as StoreEnrichmentStatus, ListQuery, OptionalTextUpdate, SearchQuery,
    StoreError, StoredItem, V2Store,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::cli::{
    CaptureCommands, CliArgs, Commands, EnrichCommands, EnrichmentProviderArg, ImportCommands,
    OutputFormat, SyncCommands, ZenCommands,
};
use crate::{capture, enrichment, sync, tui};

type CliResult<T> = Result<T, Box<dyn Error>>;

const OUTPUT_SCHEMA_VERSION: u8 = 1;
const DATABASE_FILE: &str = "library.sqlite3";
const MAX_ENRICHMENT_SECRET_BYTES: u64 = 16 * 1024;

pub async fn handle(args: &CliArgs) -> CliResult<()> {
    let data_dir = resolve_data_dir(args.data_dir.as_deref())?;

    match &args.command {
        Commands::Init => handle_init(&data_dir, args.format).await,
        Commands::Add(add) => {
            let store = V2Store::open(&data_dir).await?;
            let request = CreateItemRequest {
                url: add.url.clone(),
                title: add.title.clone(),
                excerpt: add.excerpt.clone(),
                favorite: add.favorite,
                language: add.language.clone(),
                saved_at: add.saved_at,
                note: add.note.clone().unwrap_or_default(),
                tags: add.tag.clone(),
            };
            let item = if let Some(provider) = add.enrich {
                let provider = store_provider(provider);
                let saved = store.create_item_with_enrichment(request, provider).await?;
                finish_post_save_enrichment(&store, &data_dir, saved).await
            } else {
                store.create_item(request).await?
            };
            let output = command_output("add", item)?;
            write_single(args.format, &output, human_add)
        }
        Commands::Enrich { command } => handle_enrich(&data_dir, args.format, command).await,
        Commands::Edit(edit) => {
            let store = V2Store::open(&data_dir).await?;
            let item = store
                .edit_item(EditItemRequest {
                    item_id: edit.item_id.clone(),
                    url: edit.url.clone(),
                    title: optional_text_update(&edit.title, edit.clear_title),
                    excerpt: optional_text_update(&edit.excerpt, edit.clear_excerpt),
                    favorite: edit.favorite,
                    language: optional_text_update(&edit.language, edit.clear_language),
                    saved_at: edit.saved_at,
                    note: edit.note.clone(),
                    expected_note: None,
                    add_tags: edit.add_tag.clone(),
                    remove_tags: edit.remove_tag.clone(),
                })
                .await?;
            let output = command_output("edit", item)?;
            write_single(args.format, &output, human_edit)
        }
        Commands::Delete(item) => {
            let store = V2Store::open(&data_dir).await?;
            let item = store.delete_item(&item.item_id).await?;
            let output = command_output("delete", item)?;
            write_single(args.format, &output, human_delete)
        }
        Commands::Restore(item) => {
            let store = V2Store::open(&data_dir).await?;
            let item = store.restore_item(&item.item_id).await?;
            let output = command_output("restore", item)?;
            write_single(args.format, &output, human_restore)
        }
        Commands::Import {
            command: ImportCommands::V1(import),
        } => {
            let store = V2Store::open(&data_dir).await?;
            let result = store.import_v1(&import.source).await?;
            let output = command_output("import_v1", result)?;
            report_rejections(&output);
            write_single(args.format, &output, human_import)
        }
        Commands::List(list) => {
            let store = V2Store::open(&data_dir).await?;
            let result = store
                .list(ListQuery {
                    tags: list.tags.clone(),
                    favorite_only: list.favorite_only,
                    include_deleted: list.include_deleted,
                    limit: if list.all {
                        None
                    } else {
                        Some(list.limit.unwrap_or(50))
                    },
                    offset: list.offset,
                })
                .await?;
            let output = command_output("list", result)?;
            write_list(args.format, &output)
        }
        Commands::Search(search) => {
            let store = V2Store::open(&data_dir).await?;
            let filters = &search.filters;
            let result = store
                .search(SearchQuery {
                    text: search.query.clone(),
                    tags: filters.tags.clone(),
                    favorite_only: filters.favorite_only,
                    include_deleted: filters.include_deleted,
                    limit: if filters.all {
                        None
                    } else {
                        Some(filters.limit.unwrap_or(50))
                    },
                    offset: filters.offset,
                })
                .await?;
            let output = command_output("search", result)?;
            write_search(args.format, &output)
        }
        Commands::Tui => {
            if args.format != OutputFormat::Human {
                return Err(io::Error::other("the TUI supports only human output").into());
            }
            let store = V2Store::open(&data_dir).await?;
            tui::run(&store, &data_dir).await
        }
        Commands::Capture { command } => handle_capture(&data_dir, args.format, command).await,
        Commands::Sync { command } => handle_sync(&data_dir, args.format, command).await,
        Commands::Zen { command } => handle_zen(&data_dir, args.format, command).await,
        Commands::Status => handle_status(&data_dir, args.format).await,
    }
}

/// Zen documents from the terminal.
///
/// Bodies come from a file or standard input rather than an argument, because a
/// document is prose and shells mangle prose. `--body` stays for one-liners.
async fn handle_zen(
    data_dir: &Path,
    format: OutputFormat,
    command: &ZenCommands,
) -> CliResult<()> {
    let store = V2Store::open(data_dir).await?;
    match command {
        ZenCommands::List => {
            let documents = store.list_zen_documents().await?;
            let output = command_output(
                "zen_list",
                json!({ "documents": documents, "total": documents.len() }),
            )?;
            write_single(format, &output, human_zen_list)
        }
        ZenCommands::Add(add) => {
            let summary = store
                .create_zen_document(CreateZenDocumentRequest {
                    title: add.title.clone(),
                    body: read_body(add.body.as_deref(), add.body_file.as_deref())?,
                    tags: add.tag.clone(),
                })
                .await?;
            let output = command_output("zen_add", summary)?;
            write_single(format, &output, human_zen_document)
        }
        ZenCommands::Show(document) => {
            let view = store.zen_document(&document.document_id).await?;
            let output = command_output("zen_show", view)?;
            write_single(format, &output, human_zen_show)
        }
        ZenCommands::Edit(edit) => {
            let body = match (&edit.body, &edit.body_file) {
                (None, None) => None,
                (body, file) => Some(read_body(body.as_deref(), file.as_deref())?),
            };
            let summary = store
                .edit_zen_document(EditZenDocumentRequest {
                    document_id: edit.document_id.clone(),
                    title: match (&edit.title, edit.clear_title) {
                        (_, true) => Some(None),
                        (Some(title), false) => Some(Some(title.clone())),
                        (None, false) => None,
                    },
                    body,
                    expected_body: None,
                    add_tags: edit.add_tag.clone(),
                    remove_tags: edit.remove_tag.clone(),
                })
                .await?;
            let output = command_output("zen_edit", summary)?;
            write_single(format, &output, human_zen_document)
        }
        ZenCommands::Delete(document) => {
            let summary = store.delete_zen_document(&document.document_id).await?;
            let output = command_output("zen_delete", summary)?;
            write_single(format, &output, human_zen_document)
        }
        ZenCommands::Restore(document) => {
            let summary = store.restore_zen_document(&document.document_id).await?;
            let output = command_output("zen_restore", summary)?;
            write_single(format, &output, human_zen_document)
        }
    }
}

/// Reads a document body from text, a file, or standard input via `-`.
fn read_body(body: Option<&str>, body_file: Option<&Path>) -> CliResult<String> {
    match (body, body_file) {
        (Some(body), _) => Ok(body.to_owned()),
        (None, Some(path)) if path == Path::new("-") => {
            let mut body = String::new();
            io::stdin().read_to_string(&mut body)?;
            Ok(body)
        }
        (None, Some(path)) => Ok(std::fs::read_to_string(path)?),
        (None, None) => Ok(String::new()),
    }
}

fn human_zen_list(value: &Value) {
    let Some(documents) = value.get("documents").and_then(Value::as_array) else {
        println!("No documents found");
        return;
    };
    println!("{} document{}", documents.len(), plural(documents.len()));
    for document in documents {
        println!();
        println!(
            "{}",
            document
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Untitled document")
        );
        print_indented_string(document, "document_id", "ID");
        print_zen_counts(document);
    }
}

fn human_zen_document(value: &Value) {
    println!(
        "{}",
        value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Untitled document")
    );
    print_indented_string(value, "document_id", "ID");
    print_zen_counts(value);
}

fn human_zen_show(value: &Value) {
    println!(
        "{}",
        value
            .pointer("/title/value")
            .and_then(Value::as_str)
            .unwrap_or("Untitled document")
    );
    print_indented_string(value, "document_id", "ID");
    if let Some(tags) = value.get("tags").and_then(Value::as_array) {
        let tags = tags.iter().filter_map(Value::as_str).collect::<Vec<_>>();
        if !tags.is_empty() {
            println!("  tags: {}", tags.join(", "));
        }
    }
    if let Some(body) = value.get("body").and_then(Value::as_str) {
        println!();
        println!("{body}");
    }
}

fn print_zen_counts(document: &Value) {
    if let Some(bytes) = document.get("byte_length").and_then(Value::as_u64) {
        println!("  size: {bytes} B");
    }
    let total = document.get("todo_total").and_then(Value::as_u64);
    if let Some(total) = total.filter(|total| *total > 0) {
        let done = document
            .get("todo_done")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        println!("  todos: {done}/{total}");
    }
    if let Some(tags) = document.get("tags").and_then(Value::as_array) {
        let tags = tags.iter().filter_map(Value::as_str).collect::<Vec<_>>();
        if !tags.is_empty() {
            println!("  tags: {}", tags.join(", "));
        }
    }
    if document.get("lifecycle_state").and_then(Value::as_str) == Some("deleted") {
        println!("  state: deleted");
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

async fn handle_capture(
    data_dir: &Path,
    format: OutputFormat,
    command: &CaptureCommands,
) -> CliResult<()> {
    match command {
        CaptureCommands::Install => {
            let result = capture::install(data_dir).await?;
            let output = command_output("capture_install", result)?;
            write_single(format, &output, human_capture_install)
        }
        CaptureCommands::Status => {
            let output = command_output("capture_status", capture::status()?)?;
            write_single(format, &output, human_capture_status)
        }
        CaptureCommands::Uninstall => {
            let output = command_output("capture_uninstall", capture::uninstall()?)?;
            write_single(format, &output, human_capture_uninstall)
        }
        CaptureCommands::Handle(handle) => {
            let provider = match automatic_capture_provider(data_dir) {
                Ok(provider) => provider,
                Err(error) => {
                    eprintln!(
                        "Capture will continue without metadata enrichment: {}",
                        error.kind()
                    );
                    None
                }
            };
            let item = capture::handle(data_dir, &handle.uri, handle.notify, provider).await?;
            let item = if provider.is_some() {
                let store = V2Store::open(data_dir).await?;
                finish_post_save_enrichment(&store, data_dir, item).await
            } else {
                item
            };
            if handle.notify {
                return Ok(());
            }
            let output = command_output("capture_handle", item)?;
            write_single(format, &output, human_capture_handle)
        }
    }
}

async fn handle_enrich(
    data_dir: &Path,
    format: OutputFormat,
    command: &EnrichCommands,
) -> CliResult<()> {
    // Enrichment configuration belongs to an initialized library even though
    // its secrets and retry state live outside synchronized protocol data.
    let store = V2Store::open(data_dir).await?;
    match command {
        EnrichCommands::Configure(configure) => {
            let provider = store_provider(configure.provider);
            if provider == EnrichmentProvider::Direct
                && (configure.api_key_stdin || configure.api_url.is_some())
            {
                return Err(io::Error::other(
                    "--api-key-stdin and --api-url apply only to the firecrawl provider",
                )
                .into());
            }
            let api_key = if configure.api_key_stdin {
                Some(read_secret_from_stdin()?)
            } else {
                None
            };
            let status = enrichment::configure(
                data_dir,
                provider,
                configure.on_capture,
                configure.api_url.as_deref(),
                api_key.as_deref(),
            )?;
            let output = command_output(
                "enrich_configure",
                EnrichmentStatusOutput {
                    configuration: status,
                    queue: store.enrichment_queue_counts().await?,
                },
            )?;
            write_single(format, &output, human_enrich_status)
        }
        EnrichCommands::Status => {
            let output = command_output(
                "enrich_status",
                EnrichmentStatusOutput {
                    configuration: enrichment::status(data_dir)?,
                    queue: store.enrichment_queue_counts().await?,
                },
            )?;
            write_single(format, &output, human_enrich_status)
        }
        EnrichCommands::Disable => {
            let output = command_output(
                "enrich_disable",
                EnrichmentStatusOutput {
                    configuration: enrichment::disable(data_dir)?,
                    queue: store.enrichment_queue_counts().await?,
                },
            )?;
            write_single(format, &output, human_enrich_status)
        }
        EnrichCommands::Run(run) => {
            if run.item_id.is_none() && run.provider.is_some() {
                return Err(io::Error::other(
                    "--provider requires an item ID; queued jobs retain their chosen provider",
                )
                .into());
            }

            let mut result = EnrichmentRunResult::default();
            if let Some(item_id) = run.item_id.as_deref() {
                let outcome = match explicit_enrichment_job(
                    &store,
                    data_dir,
                    item_id,
                    run.provider.map(store_provider),
                    run.replace_excerpt,
                    None,
                )
                .await?
                {
                    Some(claim) => attempt_enrichment(&store, data_dir, claim).await?,
                    None => current_enrichment_outcome(&store, item_id).await?,
                };
                result.push(outcome);
            } else {
                for _ in 0..run.limit {
                    let Some(claim) = store.claim_next_due_enrichment_job().await? else {
                        break;
                    };
                    result.push(attempt_enrichment(&store, data_dir, claim).await?);
                }
            }
            let output = command_output("enrich_run", result)?;
            write_single(format, &output, human_enrich_run)
        }
    }
}

async fn explicit_enrichment_job(
    store: &V2Store,
    data_dir: &Path,
    item_id: &str,
    requested_provider: Option<EnrichmentProvider>,
    replace_excerpt: bool,
    expected_excerpt: Option<&str>,
) -> CliResult<Option<EnrichmentClaim>> {
    let existing = store.enrichment_job(item_id).await?;
    if replace_excerpt {
        if existing
            .as_ref()
            .is_some_and(|job| job.status == StoreEnrichmentStatus::InProgress)
        {
            return Err(io::Error::other(
                "this item is already being enriched; retry after its local lease expires",
            )
            .into());
        }
        let provider = match requested_provider.or(existing.as_ref().map(|job| job.provider)) {
            Some(provider) => provider,
            None => configured_provider(data_dir)?.ok_or_else(|| {
                io::Error::other(
                    "no enrichment provider is configured; pass --provider or run enrich configure",
                )
            })?,
        };
        let queued = match expected_excerpt {
            Some(expected_excerpt) => {
                store
                    .queue_item_enrichment_replacing_excerpt_if_unchanged(
                        item_id,
                        provider,
                        expected_excerpt,
                    )
                    .await?
            }
            None => {
                store
                    .queue_item_enrichment_replacing_excerpt(item_id, provider)
                    .await?
            }
        };
        return claim_queued_enrichment(store, queued).await;
    }

    match existing {
        Some(job)
            if matches!(
                job.status,
                StoreEnrichmentStatus::Pending | StoreEnrichmentStatus::Retry
            ) =>
        {
            if requested_provider.is_some_and(|provider| provider != job.provider) {
                return Err(io::Error::other(
                    "this item already has a pending job with another provider",
                )
                .into());
            }
            Ok(Some(store.claim_item_enrichment(item_id).await?))
        }
        Some(job) if job.status == StoreEnrichmentStatus::InProgress => {
            match store.claim_item_enrichment(item_id).await {
                Ok(claim) => Ok(Some(claim)),
                Err(StoreError::EnrichmentJobNotPending(_)) => Err(io::Error::other(
                    "this item is already being enriched; retry after its local lease expires",
                )
                .into()),
                Err(error) => Err(error.into()),
            }
        }
        Some(job) if job.status == StoreEnrichmentStatus::Failed => {
            let queued = store
                .queue_item_enrichment(item_id, requested_provider.unwrap_or(job.provider))
                .await?;
            claim_queued_enrichment(store, queued).await
        }
        Some(job) => {
            let provider = requested_provider.ok_or_else(|| {
                io::Error::other(format!(
                    "this item's enrichment job is {:?}; pass --provider to queue it again",
                    job.status
                ))
            })?;
            let queued = store.queue_item_enrichment(item_id, provider).await?;
            claim_queued_enrichment(store, queued).await
        }
        None => {
            let provider = match requested_provider {
                Some(provider) => provider,
                None => configured_provider(data_dir)?.ok_or_else(|| {
                    io::Error::other(
                        "no enrichment provider is configured; pass --provider or run enrich configure",
                    )
                })?,
            };
            let queued = store.queue_item_enrichment(item_id, provider).await?;
            claim_queued_enrichment(store, queued).await
        }
    }
}

async fn claim_queued_enrichment(
    store: &V2Store,
    queued: EnrichmentJob,
) -> CliResult<Option<EnrichmentClaim>> {
    if queued.status == StoreEnrichmentStatus::Skipped {
        Ok(None)
    } else {
        Ok(Some(store.claim_item_enrichment(&queued.item_id).await?))
    }
}

async fn finish_post_save_enrichment(
    store: &V2Store,
    data_dir: &Path,
    saved: StoredItem,
) -> StoredItem {
    match attempt_queued_enrichment(store, data_dir, &saved.id).await {
        Ok(Some(outcome)) => {
            if let Some(kind) = outcome.summary.last_error_kind.as_deref() {
                if outcome.summary.status == StoreEnrichmentStatus::Retry {
                    eprintln!("Saved locally; metadata enrichment will retry: {kind}");
                } else {
                    eprintln!("Saved locally; metadata enrichment failed: {kind}");
                }
            }
            outcome.item
        }
        Ok(None) => saved,
        Err(_) => {
            eprintln!("Saved locally; metadata enrichment remains queued");
            saved
        }
    }
}

pub(crate) async fn attempt_queued_enrichment(
    store: &V2Store,
    data_dir: &Path,
    item_id: &str,
) -> CliResult<Option<EnrichmentAttemptOutcome>> {
    let Some(job) = store.enrichment_job(item_id).await? else {
        return Ok(None);
    };
    if matches!(
        job.status,
        StoreEnrichmentStatus::Pending | StoreEnrichmentStatus::Retry
    ) {
        let claim = match store.claim_item_enrichment(item_id).await {
            Ok(claim) => claim,
            Err(StoreError::EnrichmentJobNotPending(_)) => {
                return Ok(Some(current_enrichment_outcome(store, item_id).await?));
            }
            Err(error) => return Err(error.into()),
        };
        return Ok(Some(attempt_enrichment(store, data_dir, claim).await?));
    }
    Ok(Some(current_enrichment_outcome(store, item_id).await?))
}

pub(crate) async fn enrich_item_with_configured_provider(
    store: &V2Store,
    data_dir: &Path,
    item_id: &str,
    replace_excerpt: bool,
    expected_excerpt: Option<String>,
) -> CliResult<EnrichmentAttemptOutcome> {
    let existing = store.enrichment_job(item_id).await?;
    let requested_provider = if !replace_excerpt
        && existing.as_ref().is_some_and(|job| {
            matches!(
                job.status,
                StoreEnrichmentStatus::Pending
                    | StoreEnrichmentStatus::Retry
                    | StoreEnrichmentStatus::InProgress
            )
        }) {
        None
    } else {
        Some(configured_provider(data_dir)?.ok_or_else(|| {
            io::Error::other(
                "no enrichment provider is configured; run `research enrich configure direct` or configure Firecrawl",
            )
        })?)
    };
    let claim = match explicit_enrichment_job(
        store,
        data_dir,
        item_id,
        requested_provider,
        replace_excerpt,
        expected_excerpt.as_deref(),
    )
    .await
    {
        Ok(claim) => claim,
        Err(error) => return Err(error),
    };
    match claim {
        Some(claim) => attempt_enrichment(store, data_dir, claim).await,
        None => current_enrichment_outcome(store, item_id).await,
    }
}

async fn attempt_enrichment(
    store: &V2Store,
    data_dir: &Path,
    claim: EnrichmentClaim,
) -> CliResult<EnrichmentAttemptOutcome> {
    let EnrichmentClaim { job, lease_token } = claim;
    let current = store.item(&job.item_id).await?;
    if job.status != StoreEnrichmentStatus::InProgress {
        return Ok(EnrichmentAttemptOutcome {
            item: current,
            summary: enrichment_attempt_summary(job, Vec::new()),
        });
    }
    if current.state != "active" {
        let skipped = match store
            .apply_item_enrichment(
                &job.item_id,
                &lease_token,
                &current.url,
                &current.state,
                Default::default(),
            )
            .await
        {
            Ok(skipped) => skipped,
            Err(StoreError::StaleEdit | StoreError::EnrichmentJobNotPending(_)) => {
                return current_enrichment_outcome(store, &job.item_id).await;
            }
            Err(error) => return Err(error.into()),
        };
        return Ok(EnrichmentAttemptOutcome {
            item: skipped.item,
            summary: enrichment_attempt_summary(skipped.job, Vec::new()),
        });
    }
    match enrichment::extract(data_dir, job.provider, &current.url).await {
        Ok(candidates) => {
            let applied = match store
                .apply_item_enrichment(
                    &job.item_id,
                    &lease_token,
                    &current.url,
                    &current.state,
                    research_store::EnrichmentCandidates {
                        title: candidates.title,
                        excerpt: candidates.excerpt,
                        language: candidates.language,
                    },
                )
                .await
            {
                Ok(applied) => applied,
                Err(StoreError::StaleEdit) => {
                    let retry = match store
                        .record_enrichment_failure(&job.item_id, &lease_token, "stale_item")
                        .await
                    {
                        Ok(retry) => retry,
                        Err(StoreError::EnrichmentJobNotPending(_)) => {
                            return current_enrichment_outcome(store, &job.item_id).await;
                        }
                        Err(error) => return Err(error.into()),
                    };
                    return Ok(EnrichmentAttemptOutcome {
                        item: store.item(&job.item_id).await?,
                        summary: enrichment_attempt_summary(retry, Vec::new()),
                    });
                }
                Err(StoreError::EnrichmentJobNotPending(_)) => {
                    return current_enrichment_outcome(store, &job.item_id).await;
                }
                Err(error) => return Err(error.into()),
            };
            let mut applied_fields = Vec::new();
            if applied.applied_title {
                applied_fields.push("title");
            }
            if applied.applied_excerpt {
                applied_fields.push("excerpt");
            }
            if applied.applied_captured_document {
                applied_fields.push("captured_document");
            }
            if applied.applied_language {
                applied_fields.push("language");
            }
            Ok(EnrichmentAttemptOutcome {
                item: applied.item,
                summary: enrichment_attempt_summary(applied.job, applied_fields),
            })
        }
        Err(error) => {
            let failed = match store
                .record_enrichment_failure(&job.item_id, &lease_token, error.kind())
                .await
            {
                Ok(failed) => failed,
                Err(StoreError::EnrichmentJobNotPending(_)) => {
                    return current_enrichment_outcome(store, &job.item_id).await;
                }
                Err(error) => return Err(error.into()),
            };
            Ok(EnrichmentAttemptOutcome {
                item: current,
                summary: enrichment_attempt_summary(failed, Vec::new()),
            })
        }
    }
}

async fn current_enrichment_outcome(
    store: &V2Store,
    item_id: &str,
) -> CliResult<EnrichmentAttemptOutcome> {
    let job = store
        .enrichment_job(item_id)
        .await?
        .ok_or_else(|| io::Error::other("the enrichment job disappeared"))?;
    Ok(EnrichmentAttemptOutcome {
        item: store.item(item_id).await?,
        summary: enrichment_attempt_summary(job, Vec::new()),
    })
}

pub(crate) fn configured_provider(
    data_dir: &Path,
) -> enrichment::EnrichmentResult<Option<EnrichmentProvider>> {
    let status = enrichment::status(data_dir)?;
    Ok(status.configured.then_some(status.provider).flatten())
}

fn automatic_capture_provider(
    data_dir: &Path,
) -> enrichment::EnrichmentResult<Option<EnrichmentProvider>> {
    let status = enrichment::status(data_dir)?;
    Ok((status.configured && status.on_capture)
        .then_some(status.provider)
        .flatten())
}

fn store_provider(provider: EnrichmentProviderArg) -> EnrichmentProvider {
    match provider {
        EnrichmentProviderArg::Direct => EnrichmentProvider::Direct,
        EnrichmentProviderArg::Firecrawl => EnrichmentProvider::Firecrawl,
    }
}

fn read_secret_from_stdin() -> CliResult<String> {
    let mut value = String::new();
    let stdin = io::stdin();
    stdin
        .lock()
        .take(MAX_ENRICHMENT_SECRET_BYTES + 1)
        .read_to_string(&mut value)?;
    if value.len() as u64 > MAX_ENRICHMENT_SECRET_BYTES {
        return Err(io::Error::other("API key input is too large").into());
    }
    let value = value.trim_end_matches(['\r', '\n']).to_owned();
    if value.is_empty() {
        return Err(io::Error::other("standard input did not contain an API key").into());
    }
    Ok(value)
}

#[derive(Default, Serialize)]
struct EnrichmentRunResult {
    processed: usize,
    succeeded: usize,
    skipped: usize,
    retrying: usize,
    in_progress: usize,
    failed: usize,
    attempts: Vec<EnrichmentAttemptSummary>,
}

#[derive(Serialize)]
struct EnrichmentStatusOutput {
    #[serde(flatten)]
    configuration: enrichment::EnrichmentStatus,
    queue: EnrichmentQueueCounts,
}

impl EnrichmentRunResult {
    fn push(&mut self, outcome: EnrichmentAttemptOutcome) {
        self.processed += 1;
        match outcome.summary.status {
            StoreEnrichmentStatus::Succeeded => self.succeeded += 1,
            StoreEnrichmentStatus::Skipped => self.skipped += 1,
            StoreEnrichmentStatus::Retry => self.retrying += 1,
            StoreEnrichmentStatus::Failed => self.failed += 1,
            StoreEnrichmentStatus::InProgress => self.in_progress += 1,
            StoreEnrichmentStatus::Pending => {}
        }
        self.attempts.push(outcome.summary);
    }
}

pub(crate) struct EnrichmentAttemptOutcome {
    item: StoredItem,
    summary: EnrichmentAttemptSummary,
}

impl EnrichmentAttemptOutcome {
    pub(crate) fn status(&self) -> StoreEnrichmentStatus {
        self.summary.status
    }

    pub(crate) fn applied_fields(&self) -> &[&'static str] {
        &self.summary.applied_fields
    }

    pub(crate) fn last_error_kind(&self) -> Option<&str> {
        self.summary.last_error_kind.as_deref()
    }
}

#[derive(Serialize)]
struct EnrichmentAttemptSummary {
    item_id: String,
    provider: EnrichmentProvider,
    status: StoreEnrichmentStatus,
    attempts: u64,
    applied_fields: Vec<&'static str>,
    next_attempt_at: Option<String>,
    last_error_kind: Option<String>,
}

fn enrichment_attempt_summary(
    job: EnrichmentJob,
    applied_fields: Vec<&'static str>,
) -> EnrichmentAttemptSummary {
    EnrichmentAttemptSummary {
        item_id: job.item_id,
        provider: job.provider,
        status: job.status,
        attempts: job.attempts,
        applied_fields,
        next_attempt_at: job.next_attempt_at,
        last_error_kind: job.last_error_kind,
    }
}

async fn handle_sync(
    data_dir: &Path,
    format: OutputFormat,
    command: &SyncCommands,
) -> CliResult<()> {
    let store = V2Store::open(data_dir).await?;
    match command {
        SyncCommands::Connect(connect) => {
            let result =
                sync::connect(&store, &connect.repository, connect.branch.as_deref()).await?;
            let output = command_output("sync_connect", result)?;
            write_single(format, &output, human_sync_connect)
        }
        SyncCommands::Run(run) => {
            if run.every.is_some() && format == OutputFormat::Json {
                return Err(io::Error::other(
                    "--every produces a stream; use --format ndjson or human output",
                )
                .into());
            }
            let Some(seconds) = run.every else {
                let result = sync::run_once(&store).await?;
                let output = command_output("sync_run", result)?;
                return write_single(format, &output, human_sync_run);
            };
            loop {
                let retry_after = match sync::run_once(&store).await {
                    Ok(result) => {
                        let output = command_output("sync_run", result)?;
                        write_single(format, &output, human_sync_run)?;
                        None
                    }
                    Err(error) if error.is_retryable() => {
                        write_sync_retry(format, &error)?;
                        error.retry_after()
                    }
                    Err(error) => return Err(error.into()),
                };
                let delay = retry_after
                    .map(|retry_after| retry_after.max(Duration::from_secs(seconds)))
                    .unwrap_or_else(|| Duration::from_secs(seconds));
                tokio::select! {
                    result = tokio::signal::ctrl_c() => {
                        result?;
                        return Ok(());
                    }
                    () = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
}

fn write_sync_retry(format: OutputFormat, error: &sync::SyncError) -> CliResult<()> {
    match format {
        OutputFormat::Human => {
            eprintln!("Synchronization will retry: {error}");
        }
        OutputFormat::Ndjson => {
            write_json(
                &json!({
                    "schema_version": OUTPUT_SCHEMA_VERSION,
                    "command": "sync_run",
                    "type": "sync_error",
                    "error_kind": error.kind(),
                    "retryable": true
                }),
                false,
            )?;
        }
        OutputFormat::Json => {
            return Err(io::Error::other(
                "periodic synchronization cannot emit one JSON document",
            )
            .into());
        }
    }
    Ok(())
}

fn optional_text_update(value: &Option<String>, clear: bool) -> Option<OptionalTextUpdate> {
    if clear {
        Some(OptionalTextUpdate::Clear)
    } else {
        value.clone().map(OptionalTextUpdate::Set)
    }
}

async fn handle_init(data_dir: &Path, format: OutputFormat) -> CliResult<()> {
    let created = !data_dir.join(DATABASE_FILE).is_file();
    let store = V2Store::init(data_dir).await?;
    let status = store.status().await?;
    let mut output = command_output("init", status)?;
    let object = output
        .as_object_mut()
        .ok_or_else(|| io::Error::other("V2 status did not serialize as an object"))?;
    object.insert("created".into(), Value::Bool(created));
    object.insert(
        "data_dir".into(),
        Value::String(data_dir.display().to_string()),
    );
    object.insert(
        "database_path".into(),
        Value::String(store.database_path().display().to_string()),
    );
    write_single(format, &output, human_init)
}

async fn handle_status(data_dir: &Path, format: OutputFormat) -> CliResult<()> {
    if !data_dir.join(DATABASE_FILE).is_file() {
        let output = json!({
            "schema_version": OUTPUT_SCHEMA_VERSION,
            "command": "status",
            "initialized": false,
            "data_dir": data_dir.display().to_string(),
            "database_path": data_dir.join(DATABASE_FILE).display().to_string(),
            "sync_state": "not_configured"
        });
        return write_single(format, &output, human_status);
    }

    let store = V2Store::open(data_dir).await?;
    let status = store.status().await?;
    let mut output = command_output("status", status)?;
    let object = output
        .as_object_mut()
        .ok_or_else(|| io::Error::other("V2 status did not serialize as an object"))?;
    object.insert("initialized".into(), Value::Bool(true));
    object.insert(
        "data_dir".into(),
        Value::String(data_dir.display().to_string()),
    );
    object.insert(
        "database_path".into(),
        Value::String(store.database_path().display().to_string()),
    );
    write_single(format, &output, human_status)
}

fn resolve_data_dir(explicit: Option<&Path>) -> CliResult<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    let project = ProjectDirs::from("io.github", "ResearchPocket", "ResearchPocket")
        .ok_or_else(|| io::Error::other("this platform has no application data directory"))?;
    Ok(project.data_local_dir().to_path_buf())
}

fn command_output(command: &str, value: impl Serialize) -> CliResult<Value> {
    let value = serde_json::to_value(value)?;
    let mut object = match value {
        Value::Object(object) => object,
        _ => {
            return Err(io::Error::other(format!(
                "V2 {command} result did not serialize as an object"
            ))
            .into());
        }
    };
    object.insert("schema_version".into(), Value::from(OUTPUT_SCHEMA_VERSION));
    object.insert("command".into(), Value::String(command.to_owned()));
    Ok(Value::Object(object))
}

fn write_single(format: OutputFormat, value: &Value, human: fn(&Value)) -> CliResult<()> {
    match format {
        OutputFormat::Human => human(value),
        OutputFormat::Json => write_json(value, true)?,
        OutputFormat::Ndjson => write_json(value, false)?,
    }
    Ok(())
}

fn write_list(format: OutputFormat, value: &Value) -> CliResult<()> {
    write_items(format, value, human_list)
}

fn write_search(format: OutputFormat, value: &Value) -> CliResult<()> {
    write_items(format, value, human_search)
}

fn write_items(format: OutputFormat, value: &Value, human: fn(&Value)) -> CliResult<()> {
    match format {
        OutputFormat::Human => human(value),
        OutputFormat::Json => write_json(value, true)?,
        OutputFormat::Ndjson => {
            let object = value
                .as_object()
                .ok_or_else(|| io::Error::other("V2 list output is not an object"))?;
            let items = object
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| io::Error::other("V2 list output has no items array"))?;
            let page_type = if value.get("query").is_some() {
                "search_page"
            } else {
                "list_page"
            };
            let mut page = json!({
                "schema_version": OUTPUT_SCHEMA_VERSION,
                "type": page_type,
                "total": value.pointer("/page/total").cloned().unwrap_or(Value::from(items.len())),
                "offset": value.pointer("/page/offset").cloned().unwrap_or(Value::from(0)),
                "returned": value.pointer("/page/returned").cloned().unwrap_or(Value::from(items.len()))
            });
            if let Some(query) = value.get("query") {
                page["query"] = query.clone();
            }
            write_json(&page, false)?;
            for item in items {
                write_json(
                    &json!({
                        "schema_version": OUTPUT_SCHEMA_VERSION,
                        "type": "item",
                        "item": item
                    }),
                    false,
                )?;
            }
        }
    }
    Ok(())
}

fn write_json(value: &Value, pretty: bool) -> CliResult<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if pretty {
        serde_json::to_writer_pretty(&mut output, value)?;
    } else {
        serde_json::to_writer(&mut output, value)?;
    }
    writeln!(output)?;
    Ok(())
}

fn human_init(value: &Value) {
    let created = value
        .get("created")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    println!(
        "{} ResearchPocket V2 library",
        if created {
            "Initialized"
        } else {
            "Using existing"
        }
    );
    print_string(value, "library_id", "Library");
    print_string(value, "data_dir", "Data directory");
    print_string(value, "database_path", "Database");
}

fn human_add(value: &Value) {
    human_mutation("Saved", value);
}

fn human_edit(value: &Value) {
    human_mutation("Updated", value);
}

fn human_delete(value: &Value) {
    human_mutation("Deleted", value);
}

fn human_restore(value: &Value) {
    human_mutation("Restored", value);
}

fn human_enrich_status(value: &Value) {
    let configured = value
        .get("configured")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    println!(
        "Metadata enrichment: {}",
        if configured { "configured" } else { "disabled" }
    );
    print_string(value, "provider", "Provider");
    print_bool(value, "on_capture", "Automatic after browser capture");
    print_bool(value, "credential_available", "Credential available");
    print_string(value, "credential_source", "Credential source");
    print_string(value, "api_base", "API base");
    if let Some(queue) = value.get("queue") {
        print_number(queue, "pending", "Pending jobs");
        print_number(queue, "retrying", "Waiting to retry");
        print_number(queue, "in_progress", "Active jobs");
        print_number(queue, "failed", "Retries exhausted");
        print_number(queue, "succeeded", "Completed jobs");
        print_number(queue, "skipped", "Skipped jobs");
    }
}

fn human_enrich_run(value: &Value) {
    println!("Metadata enrichment complete");
    print_number(value, "processed", "Processed");
    print_number(value, "succeeded", "Enriched");
    print_number(value, "skipped", "No missing metadata found");
    print_number(value, "retrying", "Queued for retry");
    print_number(value, "in_progress", "Already active elsewhere");
    print_number(value, "failed", "Retries exhausted");
    let Some(attempts) = value.get("attempts").and_then(Value::as_array) else {
        return;
    };
    for attempt in attempts {
        let item_id = attempt
            .get("item_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown item");
        let status = attempt
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        println!("{item_id}: {status}");
        if let Some(fields) = attempt.get("applied_fields").and_then(Value::as_array) {
            let fields = fields.iter().filter_map(Value::as_str).collect::<Vec<_>>();
            if !fields.is_empty() {
                println!("  applied: {}", fields.join(", "));
            }
        }
        print_indented_string(attempt, "last_error_kind", "error");
        print_indented_string(attempt, "next_attempt_at", "next retry");
    }
}

fn human_capture_install(value: &Value) {
    println!("Installed browser capture handler");
    print_string(value, "scheme", "Scheme");
    print_string(value, "data_dir", "Library");
    print_string(value, "executable", "Executable");
}

fn human_capture_status(value: &Value) {
    let installed = value
        .get("installed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    println!(
        "Browser capture handler: {}",
        if installed {
            "installed"
        } else {
            "not installed"
        }
    );
    print_string(value, "scheme", "Scheme");
    print_string(value, "data_dir", "Library");
    print_string(value, "executable", "Executable");
}

fn human_capture_uninstall(value: &Value) {
    println!("Removed browser capture handler");
    print_string(value, "scheme", "Scheme");
}

fn human_capture_handle(value: &Value) {
    human_mutation("Saved", value);
}

fn human_mutation(action: &str, value: &Value) {
    println!("{action} item");
    print_string(value, "id", "ID");
    print_string(value, "url", "URL");
    print_string(value, "title", "Title");
    print_string(value, "saved_at", "Saved");
    print_string(value, "state", "State");
}

fn human_import(value: &Value) {
    let diagnostics = value
        .get("rejection_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    println!(
        "{}",
        if diagnostics == 0 {
            "V1 import complete"
        } else {
            "V1 import complete with migration diagnostics"
        }
    );
    print_string(value, "source_sha256", "Source SHA-256");
    print_string(value, "source_bundle_sha256", "Source bundle SHA-256");
    print_number(value, "scanned", "Scanned");
    print_number(value, "imported", "Imported");
    print_number(value, "skipped", "Skipped");
    print_number(value, "rejection_count", "Diagnostics");
    print_number(value, "tags_imported", "Distinct tags imported");
    print_bool(value, "source_unchanged", "Source unchanged");
}

fn human_sync_connect(value: &Value) {
    println!("Connected private synchronization repository");
    print_remote(value.get("remote"));
    print_bool(value, "adopted_remote_library", "Adopted remote library");
    if let Some(cycle) = value.get("cycle") {
        print_sync_counts(cycle);
    }
}

fn human_sync_run(value: &Value) {
    println!("Synchronization complete");
    print_remote(value.get("remote"));
    print_sync_counts(value);
}

fn print_remote(remote: Option<&Value>) {
    let Some(remote) = remote else {
        return;
    };
    let owner = remote.get("owner").and_then(Value::as_str);
    let repository = remote.get("repository").and_then(Value::as_str);
    if let (Some(owner), Some(repository)) = (owner, repository) {
        println!("Repository: {owner}/{repository}");
    }
    print_string(remote, "branch", "Branch");
}

fn print_sync_counts(value: &Value) {
    print_number(value, "remote_batches_seen", "Remote batches observed");
    print_number(value, "downloaded", "Downloaded");
    print_number(value, "applied", "Applied");
    print_number(value, "already_applied", "Already applied");
    print_number(value, "acknowledged", "Acknowledged");
    print_number(value, "uploaded", "Uploaded");
    print_number(value, "pending", "Pending");
    print_number(value, "aggregates_applied", "Documents applied");
    print_number(value, "aggregates_uploaded", "Documents uploaded");
    print_number(value, "aggregates_pending", "Documents pending");
}

fn human_status(value: &Value) {
    let initialized = value
        .get("initialized")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    println!(
        "ResearchPocket V2: {}",
        if initialized {
            "initialized"
        } else {
            "not initialized"
        }
    );
    print_string(value, "library_id", "Library");
    print_string(value, "device_id", "Device");
    print_string(value, "data_dir", "Data directory");
    print_string(value, "database_path", "Database");
    print_number(value, "active_items", "Active saves");
    print_number(value, "deleted_items", "Deleted saves");
    print_number(value, "pending_updates", "Pending updates");
    print_number(value, "deferred_updates", "Deferred remote updates");
    let sync_state = value
        .get("sync")
        .and_then(|sync| sync.get("state"))
        .and_then(Value::as_str)
        .or_else(|| value.get("sync_state").and_then(Value::as_str));
    if let Some(state) = sync_state {
        println!("Sync: {state}");
    }
    if let Some(remote) = value.get("sync_remote") {
        print_remote(Some(remote));
        print_string(remote, "last_success_at", "Last successful sync");
        print_string(remote, "last_error_kind", "Last sync error");
    }
}

fn human_list(value: &Value) {
    let Some(items) = value.get("items").and_then(Value::as_array) else {
        println!("No saves found");
        return;
    };
    let total = value
        .pointer("/page/total")
        .and_then(Value::as_u64)
        .unwrap_or(items.len() as u64);
    println!("Showing {} of {total} saves", items.len());

    for item in items {
        println!();
        let favorite = item
            .get("favorite")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Untitled");
        println!("{}{}", if favorite { "★ " } else { "" }, title);
        print_indented_string(item, "url", "URL");
        print_indented_string(item, "id", "ID");
        print_indented_string(item, "saved_at", "Saved");
        if let Some(tags) = item.get("tags").and_then(Value::as_array) {
            let tags = tags.iter().filter_map(Value::as_str).collect::<Vec<_>>();
            if !tags.is_empty() {
                println!("  tags: {}", tags.join(", "));
            }
        }
        if item.get("state").and_then(Value::as_str) == Some("deleted") {
            println!("  state: deleted");
        }
    }
}

fn human_search(value: &Value) {
    if let Some(query) = value.get("query").and_then(Value::as_str) {
        println!("Search: {query}");
    }
    human_list(value);
}

fn report_rejections(value: &Value) {
    let Some(rejections) = value.get("rejections").and_then(Value::as_array) else {
        return;
    };
    for rejection in rejections {
        let id = rejection
            .get("legacy_id")
            .and_then(Value::as_i64)
            .map(|id| id.to_string())
            .unwrap_or_else(|| "unknown row".to_owned());
        let reason = rejection
            .get("reason")
            .or_else(|| rejection.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("invalid record");
        eprintln!("V1 import diagnostic for row {id}: {reason}");
    }
}

fn print_string(value: &Value, field: &str, label: &str) {
    if let Some(text) = value.get(field).and_then(Value::as_str) {
        println!("{label}: {text}");
    }
}

fn print_number(value: &Value, field: &str, label: &str) {
    if let Some(number) = value.get(field).and_then(Value::as_u64) {
        println!("{label}: {number}");
    }
}

fn print_bool(value: &Value, field: &str, label: &str) {
    if let Some(flag) = value.get(field).and_then(Value::as_bool) {
        println!("{label}: {}", if flag { "yes" } else { "no" });
    }
}

fn print_indented_string(value: &Value, field: &str, label: &str) {
    if let Some(text) = value.get(field).and_then(Value::as_str) {
        println!("  {label}: {text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tui_enrichment_resumes_active_jobs_and_reconfigures_failed_jobs() {
        let directory = tempfile::tempdir().expect("temporary library");
        let store = V2Store::init(directory.path())
            .await
            .expect("initialize library");
        let item = store
            .create_item_with_enrichment(
                CreateItemRequest {
                    url: "http://127.0.0.1/private".to_owned(),
                    title: None,
                    excerpt: None,
                    favorite: false,
                    language: None,
                    saved_at: None,
                    note: String::new(),
                    tags: Vec::new(),
                },
                EnrichmentProvider::Direct,
            )
            .await
            .expect("create queued save");

        enrichment::configure(
            directory.path(),
            EnrichmentProvider::Firecrawl,
            false,
            None,
            None,
        )
        .expect("change configured provider");

        let outcome = enrich_item_with_configured_provider(
            &store,
            directory.path(),
            &item.id,
            false,
            None,
        )
        .await
        .expect("resume recorded provider");
        assert_eq!(outcome.status(), StoreEnrichmentStatus::Retry);
        assert_eq!(outcome.last_error_kind(), Some("unsafe_target"));
        assert_eq!(store.item(&item.id).await.expect("saved item"), item);
        assert_eq!(
            store.status().await.expect("store status").pending_updates,
            1
        );
        assert_eq!(
            store
                .enrichment_job(&item.id)
                .await
                .expect("job lookup")
                .expect("durable job")
                .provider,
            EnrichmentProvider::Direct
        );

        for _ in 1..5 {
            let claim = store
                .claim_item_enrichment(&item.id)
                .await
                .expect("claim retry");
            store
                .record_enrichment_failure(&item.id, &claim.lease_token, "unsafe_target")
                .await
                .expect("record retry failure");
        }
        assert_eq!(
            store
                .enrichment_job(&item.id)
                .await
                .expect("failed job lookup")
                .expect("failed durable job")
                .status,
            StoreEnrichmentStatus::Failed
        );

        let reconfigured = enrich_item_with_configured_provider(
            &store,
            directory.path(),
            &item.id,
            false,
            None,
        )
        .await
        .expect("restart failed job with configured provider");
        assert_eq!(reconfigured.status(), StoreEnrichmentStatus::Retry);
        assert_eq!(reconfigured.last_error_kind(), Some("missing_credential"));
        assert_eq!(
            store
                .enrichment_job(&item.id)
                .await
                .expect("reconfigured job lookup")
                .expect("reconfigured durable job")
                .provider,
            EnrichmentProvider::Firecrawl
        );
    }

    #[tokio::test]
    async fn tui_force_enrichment_targets_an_authored_excerpt_without_clearing_it_first() {
        let directory = tempfile::tempdir().expect("temporary library");
        let store = V2Store::init(directory.path())
            .await
            .expect("initialize library");
        let item = store
            .create_item(CreateItemRequest {
                url: "http://127.0.0.1/private".to_owned(),
                title: None,
                excerpt: Some("Authored excerpt".to_owned()),
                favorite: false,
                language: None,
                saved_at: None,
                note: String::new(),
                tags: Vec::new(),
            })
            .await
            .expect("create authored save");
        enrichment::configure(
            directory.path(),
            EnrichmentProvider::Direct,
            false,
            None,
            None,
        )
        .expect("configure provider");

        let expected_excerpt = store
            .item_excerpt_text(&item.id)
            .await
            .expect("excerpt text");
        let outcome = enrich_item_with_configured_provider(
            &store,
            directory.path(),
            &item.id,
            true,
            Some(expected_excerpt),
        )
        .await
        .expect("attempt forced enrichment");
        assert_eq!(outcome.status(), StoreEnrichmentStatus::Retry);
        assert_eq!(outcome.last_error_kind(), Some("unsafe_target"));
        assert_eq!(
            store.item(&item.id).await.expect("saved item").excerpt,
            Some("Authored excerpt".to_owned())
        );
        let job = store
            .enrichment_job(&item.id)
            .await
            .expect("job lookup")
            .expect("durable job");
        assert!(job.target_excerpt);
        assert_eq!(job.provider, EnrichmentProvider::Direct);

        let changed_item = store
            .create_item(CreateItemRequest {
                url: "http://127.0.0.1/changed".to_owned(),
                title: None,
                excerpt: Some("Excerpt shown for confirmation".to_owned()),
                favorite: false,
                language: None,
                saved_at: None,
                note: String::new(),
                tags: Vec::new(),
            })
            .await
            .expect("create changing save");
        let stale_revision = store
            .item_excerpt_text(&changed_item.id)
            .await
            .expect("confirmed excerpt text");
        store
            .edit_item(EditItemRequest {
                item_id: changed_item.id.clone(),
                excerpt: Some(OptionalTextUpdate::Set("Concurrent edit".to_owned())),
                ..EditItemRequest::default()
            })
            .await
            .expect("edit after confirmation");

        let result = enrich_item_with_configured_provider(
            &store,
            directory.path(),
            &changed_item.id,
            true,
            Some(stale_revision),
        )
        .await;
        let Err(error) = result else {
            panic!("stale confirmation must not queue replacement");
        };
        assert!(matches!(
            error.downcast_ref::<StoreError>(),
            Some(StoreError::StaleEdit)
        ));
        assert_eq!(
            store
                .item(&changed_item.id)
                .await
                .expect("concurrently edited item")
                .excerpt,
            Some("Concurrent edit".to_owned())
        );
        assert!(
            store
                .enrichment_job(&changed_item.id)
                .await
                .expect("stale job lookup")
                .is_none()
        );
    }
}
