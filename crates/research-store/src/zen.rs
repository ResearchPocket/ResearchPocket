//! Local persistence for zen documents.
//!
//! Each document is one aggregate replica in `aggregate_state`. Mutations load
//! only that replica, so editing one document never costs anything
//! proportional to the rest of the library, and every mutation atomically
//! updates the replica, its list projection, and the durable aggregate outbox.

use research_domain::{
    AggregateKind, LifecycleState, ZenAggregate, ZenDocumentSeed, ZenDocumentSummary,
    ZenDocumentView,
};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection};
use uuid::Uuid;

use crate::store::{now_rfc3339, peer_id_for_device, sha256_hex};
use crate::{StoreError, StoreResult, V2Store};

const KIND: &str = "zen_document";

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateZenDocumentRequest {
    pub title: Option<String>,
    pub body: String,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EditZenDocumentRequest {
    pub document_id: String,
    pub title: Option<Option<String>>,
    pub body: Option<String>,
    /// Reject a body replacement unless the current body still has this value.
    ///
    /// A whole-body write is spliced against whatever the replica holds now, so
    /// an editor buffer opened before a concurrent edit landed would silently
    /// undo it. Interfaces that hold a buffer send what they read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_body: Option<String>,
    pub add_tags: Vec<String>,
    pub remove_tags: Vec<String>,
}

impl EditZenDocumentRequest {
    fn has_changes(&self) -> bool {
        self.title.is_some()
            || self.body.is_some()
            || !self.add_tags.is_empty()
            || !self.remove_tags.is_empty()
    }
}

/// What a workspace index asks for. Bodies are never part of the answer.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ZenListQuery {
    pub include_deleted: bool,
}

impl V2Store {
    pub async fn create_zen_document(
        &self,
        request: CreateZenDocumentRequest,
    ) -> StoreResult<ZenDocumentSummary> {
        let document_id = Uuid::now_v7().to_string();
        let created_at = chrono::Utc::now().timestamp();
        self.commit_zen(&document_id, move |context| {
            let seed = ZenDocumentSeed {
                document_id: context.document_id.to_owned(),
                title: request.title.clone(),
                body: request.body.clone(),
                created_at,
                tags: request.tags.clone(),
            };
            let aggregate = ZenAggregate::create(&seed, context.prefix, context.peer_id)?;
            Ok(aggregate)
        })
        .await
    }

    pub async fn edit_zen_document(
        &self,
        request: EditZenDocumentRequest,
    ) -> StoreResult<ZenDocumentSummary> {
        if !request.has_changes() {
            return Err(StoreError::NoChanges);
        }
        let document_id = request.document_id.clone();
        self.commit_zen(&document_id.clone(), move |context| {
            let aggregate = context.load()?;
            if let (Some(_), Some(expected)) = (&request.body, &request.expected_body)
                && &aggregate.body()? != expected
            {
                return Err(StoreError::StaleEdit);
            }
            if let Some(title) = &request.title {
                aggregate
                    .write_title(&format!("{}/title", context.prefix), title.as_deref())?;
            }
            if let Some(body) = &request.body {
                aggregate.set_body(body)?;
            }
            for tag in &request.remove_tags {
                aggregate.remove_tag(tag)?;
            }
            for (index, tag) in request.add_tags.iter().enumerate() {
                aggregate.add_tag(tag, &format!("{}/tag-add/{index:020}", context.prefix))?;
            }
            Ok(aggregate)
        })
        .await
    }

    pub async fn delete_zen_document(
        &self,
        document_id: &str,
    ) -> StoreResult<ZenDocumentSummary> {
        self.transition_zen(document_id, LifecycleState::Deleted)
            .await
    }

    pub async fn restore_zen_document(
        &self,
        document_id: &str,
    ) -> StoreResult<ZenDocumentSummary> {
        self.transition_zen(document_id, LifecycleState::Active)
            .await
    }

    async fn transition_zen(
        &self,
        document_id: &str,
        state: LifecycleState,
    ) -> StoreResult<ZenDocumentSummary> {
        self.commit_zen(document_id, move |context| {
            let aggregate = context.load()?;
            aggregate.transition_lifecycle(&format!("{}/lifecycle", context.prefix), state)?;
            Ok(aggregate)
        })
        .await
    }

    /// Reads one document including its body.
    pub async fn zen_document(&self, document_id: &str) -> StoreResult<ZenDocumentView> {
        let mut connection = self.pool.acquire().await?;
        let peer_id = peer_id_for_device(&self.meta("device_id").await?)?;
        load_aggregate(&mut connection, document_id, peer_id)
            .await?
            .view()
            .map_err(StoreError::from)
    }

    /// Lists active documents from the projection, never touching a body.
    pub async fn list_zen_documents(&self) -> StoreResult<Vec<ZenDocumentSummary>> {
        self.list_zen_documents_with(ZenListQuery::default()).await
    }

    /// Lists documents from the projection, never touching a body.
    pub async fn list_zen_documents_with(
        &self,
        query: ZenListQuery,
    ) -> StoreResult<Vec<ZenDocumentSummary>> {
        let rows = sqlx::query(
            "SELECT document_id, title, byte_length, todo_total, todo_done, created_at, \
             lifecycle_state FROM zen_documents \
             WHERE (? OR lifecycle_state = 'active') \
             ORDER BY edited_at DESC, document_id ASC",
        )
        .bind(query.include_deleted)
        .fetch_all(&self.pool)
        .await?;
        let mut summaries = Vec::with_capacity(rows.len());
        for row in rows {
            let document_id: String = row.try_get("document_id")?;
            let tags = sqlx::query_scalar::<_, String>(
                "SELECT tag FROM zen_document_tags WHERE document_id = ? ORDER BY tag ASC",
            )
            .bind(&document_id)
            .fetch_all(&self.pool)
            .await?;
            summaries.push(ZenDocumentSummary {
                document_id,
                title: row.try_get("title")?,
                created_at: row.try_get("created_at")?,
                byte_length: usize::try_from(row.try_get::<i64, _>("byte_length")?)
                    .map_err(|_| StoreError::NumericRange("zen body length"))?,
                todo_total: usize::try_from(row.try_get::<i64, _>("todo_total")?)
                    .map_err(|_| StoreError::NumericRange("zen todo total"))?,
                todo_done: usize::try_from(row.try_get::<i64, _>("todo_done")?)
                    .map_err(|_| StoreError::NumericRange("zen todo done"))?,
                tags,
                lifecycle_state: match row.try_get::<String, _>("lifecycle_state")?.as_str() {
                    "deleted" => LifecycleState::Deleted,
                    _ => LifecycleState::Active,
                },
            });
        }
        Ok(summaries)
    }

    /// Runs one zen mutation atomically: replica, projection, and outbox.
    async fn commit_zen<F>(
        &self,
        document_id: &str,
        mutate: F,
    ) -> StoreResult<ZenDocumentSummary>
    where
        F: FnOnce(&ZenContext<'_>) -> StoreResult<ZenAggregate> + Send,
    {
        let library_id = self.meta("library_id").await?;
        let device_id = self.meta("device_id").await?;
        let peer_id = peer_id_for_device(&device_id)?;

        let mut connection = self.pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await?;
        let result = async {
            let sequence_text: String =
                sqlx::query_scalar("SELECT next_sequence FROM devices WHERE device_id = ?")
                    .bind(&device_id)
                    .fetch_one(&mut *connection)
                    .await?;
            let sequence = sequence_text
                .parse::<u64>()
                .map_err(|_| StoreError::InvalidStore("invalid device sequence".into()))?;
            let prefix = format!("{device_id}/{sequence_text}/zen/{document_id}");

            let existing = read_snapshot(&mut connection, document_id).await?;
            let before = existing
                .as_ref()
                .map(|snapshot| {
                    ZenAggregate::from_snapshot(document_id, snapshot, peer_id)
                        .map(|aggregate| aggregate.version())
                })
                .transpose()?
                .unwrap_or_default();
            let context = ZenContext {
                document_id,
                prefix: &prefix,
                peer_id,
                snapshot: existing.as_deref(),
            };
            let aggregate = mutate(&context)?;

            let now = now_rfc3339();
            let envelope =
                aggregate.export_envelope(&before, &library_id, &device_id, sequence, &now)?;
            let snapshot = aggregate.export_snapshot()?;
            let view = aggregate.view()?;
            let summary = view.summary();

            sqlx::query(
                "INSERT INTO aggregate_state \
                 (aggregate_kind, aggregate_id, snapshot, snapshot_sha256, updated_at) \
                 VALUES (?, ?, ?, ?, ?) \
                 ON CONFLICT(aggregate_kind, aggregate_id) DO UPDATE SET \
                 snapshot = excluded.snapshot, snapshot_sha256 = excluded.snapshot_sha256, \
                 updated_at = excluded.updated_at",
            )
            .bind(KIND)
            .bind(document_id)
            .bind(&snapshot)
            .bind(sha256_hex(&snapshot))
            .bind(&now)
            .execute(&mut *connection)
            .await?;
            persist_zen_projection(&mut connection, &summary, &now).await?;

            let path = envelope.path()?;
            sqlx::query(
                "INSERT INTO aggregate_batches \
                 (aggregate_kind, aggregate_id, device_id, sequence, path, payload_sha256, \
                  envelope_json, origin, applied_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, 'local', ?)",
            )
            .bind(KIND)
            .bind(document_id)
            .bind(&envelope.device_id)
            .bind(&envelope.sequence)
            .bind(&path)
            .bind(&envelope.payload_sha256)
            .bind(serde_json::to_string(&envelope)?)
            .bind(&now)
            .execute(&mut *connection)
            .await?;
            sqlx::query(
                "INSERT INTO aggregate_outbox \
                 (aggregate_kind, aggregate_id, device_id, sequence, enqueued_at) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(KIND)
            .bind(document_id)
            .bind(&envelope.device_id)
            .bind(&envelope.sequence)
            .bind(&now)
            .execute(&mut *connection)
            .await?;
            let next = sequence
                .checked_add(1)
                .ok_or(StoreError::NumericRange("device sequence"))?;
            sqlx::query("UPDATE devices SET next_sequence = ? WHERE device_id = ?")
                .bind(format!("{next:020}"))
                .bind(&device_id)
                .execute(&mut *connection)
                .await?;
            Ok(summary)
        }
        .await;
        match result {
            Ok(summary) => {
                sqlx::query("COMMIT").execute(&mut *connection).await?;
                Ok(summary)
            }
            Err(error) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
                Err(error)
            }
        }
    }
}

/// What a zen mutation is given: identity, a unique operation prefix, and the
/// replica it is editing.
struct ZenContext<'a> {
    document_id: &'a str,
    prefix: &'a str,
    peer_id: u64,
    snapshot: Option<&'a [u8]>,
}

impl ZenContext<'_> {
    fn load(&self) -> StoreResult<ZenAggregate> {
        let snapshot = self
            .snapshot
            .ok_or_else(|| StoreError::ItemNotFound(self.document_id.to_owned()))?;
        ZenAggregate::from_snapshot(self.document_id, snapshot, self.peer_id)
            .map_err(StoreError::from)
    }
}

async fn read_snapshot(
    connection: &mut SqliteConnection,
    document_id: &str,
) -> StoreResult<Option<Vec<u8>>> {
    Ok(sqlx::query_scalar(
        "SELECT snapshot FROM aggregate_state WHERE aggregate_kind = ? AND aggregate_id = ?",
    )
    .bind(KIND)
    .bind(document_id)
    .fetch_optional(&mut *connection)
    .await?)
}

async fn load_aggregate(
    connection: &mut SqliteConnection,
    document_id: &str,
    peer_id: u64,
) -> StoreResult<ZenAggregate> {
    let snapshot = read_snapshot(connection, document_id)
        .await?
        .ok_or_else(|| StoreError::ItemNotFound(document_id.to_owned()))?;
    ZenAggregate::from_snapshot(document_id, &snapshot, peer_id).map_err(StoreError::from)
}

pub(crate) async fn persist_zen_projection(
    connection: &mut SqliteConnection,
    summary: &ZenDocumentSummary,
    now: &str,
) -> StoreResult<()> {
    sqlx::query(
        "INSERT INTO zen_documents \
         (document_id, title, byte_length, todo_total, todo_done, created_at, edited_at, \
          lifecycle_state) VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(document_id) DO UPDATE SET title = excluded.title, \
         byte_length = excluded.byte_length, todo_total = excluded.todo_total, \
         todo_done = excluded.todo_done, edited_at = excluded.edited_at, \
         lifecycle_state = excluded.lifecycle_state",
    )
    .bind(&summary.document_id)
    .bind(&summary.title)
    .bind(i64::try_from(summary.byte_length).unwrap_or(i64::MAX))
    .bind(i64::try_from(summary.todo_total).unwrap_or(i64::MAX))
    .bind(i64::try_from(summary.todo_done).unwrap_or(i64::MAX))
    .bind(summary.created_at)
    .bind(now)
    .bind(match summary.lifecycle_state {
        LifecycleState::Active => "active",
        LifecycleState::Deleted => "deleted",
    })
    .execute(&mut *connection)
    .await?;
    sqlx::query("DELETE FROM zen_document_tags WHERE document_id = ?")
        .bind(&summary.document_id)
        .execute(&mut *connection)
        .await?;
    for tag in &summary.tags {
        sqlx::query("INSERT INTO zen_document_tags (document_id, tag) VALUES (?, ?)")
            .bind(&summary.document_id)
            .bind(tag)
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

/// Exposed so the aggregate kind is named once.
pub fn zen_aggregate_kind() -> AggregateKind {
    AggregateKind::Zen
}
