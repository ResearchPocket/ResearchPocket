use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, FixedOffset};
use research_domain::{
    CanonicalProjection, CheckpointArtifact, CheckpointCoverage, CoverageInterval, Library,
    LibraryGenesis, UpdateEnvelope, coverage_contains, create_checkpoint,
    unpack_operation_pack, validate_checkpoint,
};
use sqlx::{Row, SqliteConnection};

use crate::import::{persist_item_projection, persist_projection};
use crate::store::{now_rfc3339, peer_id_for_device, sha256_hex};
use crate::{
    PendingBatch, PendingCheckpoint, RemoteBatchDisposition, RemoteBatchResult,
    RemoteCheckpointResult, RemotePackResult, StoreError, StoreResult, SyncConfiguration,
    SyncIdentity, V2Store,
};

pub const CHECKPOINT_BATCH_THRESHOLD: u64 = 100;
pub const CHECKPOINT_PAYLOAD_THRESHOLD: u64 = 2 * 1024 * 1024;

impl V2Store {
    pub async fn sync_identity(&self) -> StoreResult<SyncIdentity> {
        Ok(SyncIdentity {
            library_id: self.meta("library_id").await?,
            device_id: self.meta("device_id").await?,
            pristine: self.is_pristine().await?,
        })
    }

    pub async fn sync_genesis(&self) -> StoreResult<LibraryGenesis> {
        Ok(LibraryGenesis::new(
            &self.meta("library_id").await?,
            &now_rfc3339(),
        )?)
    }

    pub async fn sync_configuration(&self) -> StoreResult<Option<SyncConfiguration>> {
        let row = sqlx::query(
            "SELECT repository_owner, repository_name, branch, configured_at, \
             last_success_at, last_error_kind, last_error_at \
             FROM sync_config WHERE singleton = 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(SyncConfiguration {
                owner: row.try_get("repository_owner")?,
                repository: row.try_get("repository_name")?,
                branch: row.try_get("branch")?,
                configured_at: row.try_get("configured_at")?,
                last_success_at: row.try_get("last_success_at")?,
                last_error_kind: row.try_get("last_error_kind")?,
                last_error_at: row.try_get("last_error_at")?,
            })
        })
        .transpose()
    }

    pub async fn configure_sync(
        &self,
        owner: &str,
        repository: &str,
        branch: &str,
    ) -> StoreResult<SyncConfiguration> {
        for (label, value) in [
            ("repository owner", owner),
            ("repository name", repository),
            ("branch", branch),
        ] {
            if value.trim().is_empty() {
                return Err(StoreError::InvalidInput(format!("{label} cannot be blank")));
            }
        }
        if let Some(existing) = self.sync_configuration().await? {
            if existing.owner == owner
                && existing.repository == repository
                && existing.branch == branch
            {
                return Ok(existing);
            }
            return Err(StoreError::InvalidInput(
                "this library is already connected to another synchronization remote".into(),
            ));
        }
        let now = now_rfc3339();
        sqlx::query(
            "INSERT INTO sync_config \
             (singleton, repository_owner, repository_name, branch, configured_at) \
             VALUES (1, ?, ?, ?, ?)",
        )
        .bind(owner)
        .bind(repository)
        .bind(branch)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        self.sync_configuration()
            .await?
            .ok_or(StoreError::SyncNotConfigured)
    }

    pub async fn adopt_library_id_if_pristine(
        &self,
        remote_library_id: &str,
    ) -> StoreResult<bool> {
        LibraryGenesis::new(remote_library_id, &now_rfc3339())?;
        let mut connection = self.pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await?;
        let result = adopt_library_id(&mut connection, remote_library_id).await;
        match result {
            Ok(adopted) => {
                sqlx::query("COMMIT").execute(&mut *connection).await?;
                Ok(adopted)
            }
            Err(error) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
                Err(error)
            }
        }
    }

    pub async fn pending_batches(&self) -> StoreResult<Vec<PendingBatch>> {
        let rows = sqlx::query(
            "SELECT b.device_id, b.sequence, b.path, b.payload_sha256, b.envelope_json, \
             o.attempts FROM outbox o JOIN batches b USING (device_id, sequence) \
             ORDER BY o.enqueued_at ASC, b.device_id ASC, b.sequence ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let attempts: i64 = row.try_get("attempts")?;
                Ok(PendingBatch {
                    device_id: row.try_get("device_id")?,
                    sequence: row.try_get("sequence")?,
                    path: row.try_get("path")?,
                    payload_sha256: row.try_get("payload_sha256")?,
                    envelope_json: row.try_get("envelope_json")?,
                    attempts: u64::try_from(attempts)
                        .map_err(|_| StoreError::NumericRange("outbox attempts"))?,
                })
            })
            .collect()
    }

    pub async fn checkpoint_candidate(
        &self,
        force: bool,
    ) -> StoreResult<Option<PendingCheckpoint>> {
        let mut connection = self.pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await?;
        let result = build_checkpoint_candidate(&mut connection, force).await;
        match result {
            Ok(candidate) => {
                sqlx::query("COMMIT").execute(&mut *connection).await?;
                Ok(candidate)
            }
            Err(error) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
                Err(error)
            }
        }
    }

    pub async fn receive_remote_checkpoint(
        &self,
        path: &str,
        blob_sha: &str,
        bytes: &[u8],
    ) -> StoreResult<RemoteCheckpointResult> {
        validate_blob_sha(blob_sha)?;
        let checkpoint_json = std::str::from_utf8(bytes)
            .map_err(|_| StoreError::SyncIntegrity(format!("{path} is not UTF-8 JSON")))?;
        let library_id = self.meta("library_id").await?;
        let artifact = validate_checkpoint(path, checkpoint_json, &library_id)?;
        let local_device_id = self.meta("device_id").await?;
        let peer_id = peer_id_for_device(&local_device_id)?;
        let snapshot = STANDARD.decode(&artifact.snapshot_base64).map_err(|_| {
            StoreError::SyncIntegrity("checkpoint payload is not Base64".into())
        })?;

        let mut connection = self.pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await?;
        let result = async {
            let pristine = store_is_pristine(&mut connection).await?;
            let restored = if pristine {
                let library = Library::from_snapshot(&snapshot, peer_id)?;
                let projection = library.canonical_projection()?;
                sqlx::query("DELETE FROM item_tags")
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("DELETE FROM item_search")
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("DELETE FROM items")
                    .execute(&mut *connection)
                    .await?;
                persist_projection(&mut connection, &projection).await?;
                sqlx::query(
                    "UPDATE canonical_state SET snapshot = ?, snapshot_sha256 = ?, updated_at = ? \
                     WHERE singleton = 1",
                )
                .bind(&snapshot)
                .bind(&artifact.checkpoint_id)
                .bind(now_rfc3339())
                .execute(&mut *connection)
                .await?;
                true
            } else {
                false
            };
            persist_checkpoint(&mut connection, &artifact, "remote").await?;
            if restored
                || coverage_is_locally_applied(&mut connection, &artifact.coverage).await?
            {
                select_checkpoint(&mut connection, &artifact).await?;
            }
            observe_remote(&mut connection, path, blob_sha).await?;
            Ok(RemoteCheckpointResult {
                batch_count: artifact.batch_count,
                restored,
            })
        }
        .await;
        match result {
            Ok(result) => {
                sqlx::query("COMMIT").execute(&mut *connection).await?;
                Ok(result)
            }
            Err(error) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
                Err(error)
            }
        }
    }

    pub async fn batch_is_checkpoint_covered(
        &self,
        device_id: &str,
        sequence: &str,
    ) -> StoreResult<bool> {
        checkpoint_covers(&self.pool, device_id, sequence).await
    }

    pub async fn remote_blob_is_current(
        &self,
        path: &str,
        blob_sha: &str,
    ) -> StoreResult<bool> {
        let observed = self.observed_remote_blob(path).await?;
        Ok(observed.as_deref() == Some(blob_sha))
    }

    pub async fn observed_remote_blob(&self, path: &str) -> StoreResult<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT blob_sha FROM remote_observations WHERE path = ?")
                .bind(path)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn record_immutable_remote_blob(
        &self,
        path: &str,
        blob_sha: &str,
    ) -> StoreResult<()> {
        validate_blob_sha(blob_sha)?;
        sqlx::query(
            "INSERT OR IGNORE INTO remote_observations (path, blob_sha, observed_at) \
             VALUES (?, ?, ?)",
        )
        .bind(path)
        .bind(blob_sha)
        .bind(now_rfc3339())
        .execute(&self.pool)
        .await?;
        let observed = self.observed_remote_blob(path).await?.ok_or_else(|| {
            StoreError::InvalidStore("remote observation was not stored".into())
        })?;
        if observed != blob_sha {
            return Err(StoreError::SyncIntegrity(
                "an immutable remote path changed after it was observed".into(),
            ));
        }
        Ok(())
    }

    pub async fn receive_remote_batch(
        &self,
        path: &str,
        blob_sha: &str,
        bytes: &[u8],
    ) -> StoreResult<RemoteBatchResult> {
        validate_blob_sha(blob_sha)?;
        let envelope_json = std::str::from_utf8(bytes)
            .map_err(|_| StoreError::SyncIntegrity(format!("{path} is not UTF-8 JSON")))?;
        let envelope: UpdateEnvelope = serde_json::from_slice(bytes)?;
        let library_id = self.meta("library_id").await?;
        let local_device_id = self.meta("device_id").await?;
        let peer_id = peer_id_for_device(&local_device_id)?;
        envelope.validate_identity(&library_id, path)?;
        validate_timestamp(&envelope.created_at, "operation creation time")?;

        let mut connection = self.pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await?;
        let result = async {
            let result =
                apply_remote_batch(&mut connection, path, envelope_json, &envelope, peer_id)
                    .await?;
            observe_remote(&mut connection, path, blob_sha).await?;
            Ok(result)
        }
        .await;
        match result {
            Ok(result) => {
                sqlx::query("COMMIT").execute(&mut *connection).await?;
                Ok(result)
            }
            Err(error) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
                Err(error)
            }
        }
    }

    pub async fn receive_remote_pack(
        &self,
        path: &str,
        blob_sha: &str,
        bytes: &[u8],
    ) -> StoreResult<RemotePackResult> {
        validate_blob_sha(blob_sha)?;
        let pack_json = std::str::from_utf8(bytes)
            .map_err(|_| StoreError::SyncIntegrity(format!("{path} is not UTF-8 JSON")))?;
        let artifact = unpack_operation_pack(path, pack_json)?;
        let expected_library_id = self.meta("library_id").await?;
        let local_device_id = self.meta("device_id").await?;
        let peer_id = peer_id_for_device(&local_device_id)?;
        let mut members = Vec::with_capacity(artifact.member_envelopes.len());
        for envelope_json in artifact.member_envelopes {
            let envelope: UpdateEnvelope = serde_json::from_str(&envelope_json)?;
            let member_path = envelope.path();
            envelope.validate_identity(&expected_library_id, &member_path)?;
            validate_timestamp(&envelope.created_at, "operation creation time")?;
            members.push((member_path, envelope_json, envelope));
        }

        let mut connection = self.pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await?;
        let result = async {
            let mut applied = 0_u64;
            let mut already_applied = 0_u64;
            let mut acknowledged_outbox = 0_u64;
            for (member_path, envelope_json, envelope) in &members {
                let member = apply_remote_batch(
                    &mut connection,
                    member_path,
                    envelope_json,
                    envelope,
                    peer_id,
                )
                .await?;
                match member.disposition {
                    RemoteBatchDisposition::Applied => applied += 1,
                    RemoteBatchDisposition::AlreadyApplied => already_applied += 1,
                }
                if member.acknowledged_outbox {
                    acknowledged_outbox += 1;
                }
            }
            observe_remote(&mut connection, path, blob_sha).await?;
            Ok(RemotePackResult {
                member_count: u64::try_from(members.len())
                    .map_err(|_| StoreError::NumericRange("operation pack member count"))?,
                applied,
                already_applied,
                acknowledged_outbox,
            })
        }
        .await;
        match result {
            Ok(result) => {
                sqlx::query("COMMIT").execute(&mut *connection).await?;
                Ok(result)
            }
            Err(error) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
                Err(error)
            }
        }
    }

    /// Re-offer persisted envelopes whose predecessors may have arrived later.
    ///
    /// An earlier sync can have observed every remote blob while leaving a
    /// reverse-ordered dependency chain deferred. Running this independently of
    /// download discovery lets the next sync heal that local intermediate state.
    pub async fn retry_deferred_batches(&self) -> StoreResult<u64> {
        let deferred: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM deferred_batches")
            .fetch_one(&self.pool)
            .await?;
        if deferred == 0 {
            return Ok(0);
        }

        let local_device_id = self.meta("device_id").await?;
        let peer_id = peer_id_for_device(&local_device_id)?;
        let mut connection = self.pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await?;
        let result = async {
            let state = sqlx::query(
                "SELECT snapshot, snapshot_sha256 FROM canonical_state WHERE singleton = 1",
            )
            .fetch_one(&mut *connection)
            .await?;
            let snapshot: Vec<u8> = state.try_get("snapshot")?;
            let expected_snapshot_sha256: String = state.try_get("snapshot_sha256")?;
            if sha256_hex(&snapshot) != expected_snapshot_sha256 {
                return Err(StoreError::InvalidStore(
                    "canonical snapshot checksum mismatch".into(),
                ));
            }
            let library = Library::from_snapshot(&snapshot, peer_id)?;
            let before_projection = library.canonical_projection()?;
            let remaining = replay_deferred_batches(&mut connection, &library).await?;
            persist_library_state(&mut connection, &library, &before_projection).await?;
            Ok(remaining)
        }
        .await;
        match result {
            Ok(remaining) => {
                sqlx::query("COMMIT").execute(&mut *connection).await?;
                Ok(remaining)
            }
            Err(error) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
                Err(error)
            }
        }
    }

    pub async fn record_outbox_attempt(
        &self,
        path: &str,
        error_kind: Option<&str>,
    ) -> StoreResult<()> {
        if let Some(kind) = error_kind {
            validate_error_kind(kind)?;
        }
        sqlx::query(
            "UPDATE outbox SET attempts = attempts + 1, last_error = ? \
             WHERE EXISTS (SELECT 1 FROM batches b WHERE b.device_id = outbox.device_id \
             AND b.sequence = outbox.sequence AND b.path = ?)",
        )
        .bind(error_kind)
        .bind(path)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_outbox_attempts(
        &self,
        paths: &[String],
        error_kind: Option<&str>,
    ) -> StoreResult<()> {
        if let Some(kind) = error_kind {
            validate_error_kind(kind)?;
        }
        let mut connection = self.pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await?;
        let result = async {
            for path in paths {
                sqlx::query(
                    "UPDATE outbox SET attempts = attempts + 1, last_error = ? \
                     WHERE EXISTS (SELECT 1 FROM batches b \
                     WHERE b.device_id = outbox.device_id \
                     AND b.sequence = outbox.sequence AND b.path = ?)",
                )
                .bind(error_kind)
                .bind(path)
                .execute(&mut *connection)
                .await?;
            }
            StoreResult::Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                sqlx::query("COMMIT").execute(&mut *connection).await?;
                Ok(())
            }
            Err(error) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
                Err(error)
            }
        }
    }

    pub async fn record_sync_success(&self) -> StoreResult<()> {
        let result = sqlx::query(
            "UPDATE sync_config SET last_success_at = ?, last_error_kind = NULL, \
             last_error_at = NULL WHERE singleton = 1",
        )
        .bind(now_rfc3339())
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(StoreError::SyncNotConfigured);
        }
        Ok(())
    }

    pub async fn record_sync_failure(&self, error_kind: &str) -> StoreResult<()> {
        validate_error_kind(error_kind)?;
        sqlx::query(
            "UPDATE sync_config SET last_error_kind = ?, last_error_at = ? \
             WHERE singleton = 1",
        )
        .bind(error_kind)
        .bind(now_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn is_pristine(&self) -> StoreResult<bool> {
        let count: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM batches) + (SELECT COUNT(*) FROM items) + \
             (SELECT COUNT(*) FROM import_rows) + (SELECT COUNT(*) FROM outbox)",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(count == 0)
    }
}

async fn adopt_library_id(
    connection: &mut SqliteConnection,
    remote_library_id: &str,
) -> StoreResult<bool> {
    let current: String =
        sqlx::query_scalar("SELECT value FROM store_meta WHERE key = 'library_id'")
            .fetch_one(&mut *connection)
            .await?;
    if current == remote_library_id {
        return Ok(false);
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM batches) + (SELECT COUNT(*) FROM items) + \
         (SELECT COUNT(*) FROM import_rows) + (SELECT COUNT(*) FROM outbox)",
    )
    .fetch_one(&mut *connection)
    .await?;
    let next_sequence: String = sqlx::query_scalar(
        "SELECT next_sequence FROM devices WHERE device_id = \
         (SELECT value FROM store_meta WHERE key = 'device_id')",
    )
    .fetch_one(&mut *connection)
    .await?;
    if count != 0 || next_sequence != "00000000000000000001" {
        return Err(StoreError::SyncLibraryMismatch(
            remote_library_id.to_owned(),
        ));
    }
    sqlx::query("UPDATE store_meta SET value = ? WHERE key = 'library_id'")
        .bind(remote_library_id)
        .execute(&mut *connection)
        .await?;
    Ok(true)
}

async fn apply_remote_batch(
    connection: &mut SqliteConnection,
    path: &str,
    envelope_json: &str,
    envelope: &UpdateEnvelope,
    peer_id: u64,
) -> StoreResult<RemoteBatchResult> {
    let existing = sqlx::query(
        "SELECT envelope_json, payload_sha256 FROM batches \
         WHERE device_id = ? AND sequence = ?",
    )
    .bind(&envelope.device_id)
    .bind(&envelope.sequence)
    .fetch_optional(&mut *connection)
    .await?;
    if let Some(existing) = existing {
        let stored_json: String = existing.try_get("envelope_json")?;
        let stored_payload_sha256: String = existing.try_get("payload_sha256")?;
        if stored_json.as_bytes() != envelope_json.as_bytes()
            || stored_payload_sha256 != envelope.payload_sha256
        {
            return Err(StoreError::SyncIntegrity(format!(
                "batch identity collision at {path}"
            )));
        }
        let acknowledged_outbox = remove_outbox(connection, envelope).await?;
        return Ok(RemoteBatchResult {
            disposition: RemoteBatchDisposition::AlreadyApplied,
            acknowledged_outbox,
        });
    }
    if checkpoint_covers(&mut *connection, &envelope.device_id, &envelope.sequence).await? {
        return Ok(RemoteBatchResult {
            disposition: RemoteBatchDisposition::AlreadyApplied,
            acknowledged_outbox: false,
        });
    }

    let state = sqlx::query(
        "SELECT snapshot, snapshot_sha256 FROM canonical_state WHERE singleton = 1",
    )
    .fetch_one(&mut *connection)
    .await?;
    let snapshot: Vec<u8> = state.try_get("snapshot")?;
    let expected_snapshot_sha256: String = state.try_get("snapshot_sha256")?;
    if sha256_hex(&snapshot) != expected_snapshot_sha256 {
        return Err(StoreError::InvalidStore(
            "canonical snapshot checksum mismatch".into(),
        ));
    }
    let library = Library::from_snapshot(&snapshot, peer_id)?;
    let before_projection = library.canonical_projection()?;
    let mut incoming_pending = library.import_envelope_has_pending(envelope)?;
    replay_deferred_batches(connection, &library).await?;
    if incoming_pending {
        incoming_pending = library.import_envelope_has_pending(envelope)?;
        if !incoming_pending {
            replay_deferred_batches(connection, &library).await?;
        }
    }
    let now = persist_library_state(connection, &library, &before_projection).await?;
    sqlx::query(
        "INSERT INTO batches \
         (device_id, sequence, payload_sha256, protocol_version, library_id, path, \
          envelope_json, origin, applied_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'remote', ?)",
    )
    .bind(&envelope.device_id)
    .bind(&envelope.sequence)
    .bind(&envelope.payload_sha256)
    .bind(i64::from(envelope.protocol_version))
    .bind(&envelope.library_id)
    .bind(path)
    .bind(envelope_json)
    .bind(&now)
    .execute(&mut *connection)
    .await?;
    if incoming_pending {
        sqlx::query("INSERT INTO deferred_batches (device_id, sequence) VALUES (?, ?)")
            .bind(&envelope.device_id)
            .bind(&envelope.sequence)
            .execute(&mut *connection)
            .await?;
    }
    Ok(RemoteBatchResult {
        disposition: RemoteBatchDisposition::Applied,
        acknowledged_outbox: false,
    })
}

/// Retry the deferred set in rounds because one deferred envelope can satisfy
/// another that sorted before it. A round with no reduction is the fixed point
/// for the currently available history.
async fn replay_deferred_batches(
    connection: &mut SqliteConnection,
    library: &Library,
) -> StoreResult<u64> {
    let rows = sqlx::query(
        "SELECT b.device_id, b.sequence, b.envelope_json FROM deferred_batches d \
         JOIN batches b USING (device_id, sequence) \
         ORDER BY b.device_id ASC, b.sequence ASC",
    )
    .fetch_all(&mut *connection)
    .await?;
    let mut pending = rows
        .into_iter()
        .map(|row| {
            let device_id: String = row.try_get("device_id")?;
            let sequence: String = row.try_get("sequence")?;
            let envelope_json: String = row.try_get("envelope_json")?;
            let envelope = serde_json::from_str(&envelope_json)?;
            StoreResult::Ok((device_id, sequence, envelope))
        })
        .collect::<StoreResult<Vec<_>>>()?;

    loop {
        if pending.is_empty() {
            return Ok(0);
        }
        let previous_count = pending.len();
        let mut unresolved = Vec::new();
        for (device_id, sequence, envelope) in pending {
            if library.import_envelope_has_pending(&envelope)? {
                unresolved.push((device_id, sequence, envelope));
            } else {
                sqlx::query(
                    "DELETE FROM deferred_batches WHERE device_id = ? AND sequence = ?",
                )
                .bind(device_id)
                .bind(sequence)
                .execute(&mut *connection)
                .await?;
            }
        }
        if unresolved.len() == previous_count {
            return u64::try_from(unresolved.len())
                .map_err(|_| StoreError::NumericRange("deferred batch count"));
        }
        pending = unresolved;
    }
}

async fn persist_library_state(
    connection: &mut SqliteConnection,
    library: &Library,
    before_projection: &CanonicalProjection,
) -> StoreResult<String> {
    let new_snapshot = library.export_snapshot()?;
    let projection = library.canonical_projection()?;
    let now = now_rfc3339();
    for (item_id, item) in &projection.items {
        if before_projection.items.get(item_id) != Some(item) {
            persist_item_projection(connection, item_id, item).await?;
        }
    }
    sqlx::query(
        "UPDATE canonical_state SET snapshot = ?, snapshot_sha256 = ?, updated_at = ? \
         WHERE singleton = 1",
    )
    .bind(&new_snapshot)
    .bind(sha256_hex(&new_snapshot))
    .bind(&now)
    .execute(&mut *connection)
    .await?;
    Ok(now)
}

pub(crate) async fn build_checkpoint_candidate(
    connection: &mut SqliteConnection,
    force: bool,
) -> StoreResult<Option<PendingCheckpoint>> {
    if sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM deferred_batches")
        .fetch_one(&mut *connection)
        .await?
        != 0
    {
        return Ok(None);
    }
    let selected_coverage = selected_coverage(&mut *connection).await?;
    let rows = sqlx::query(
        "SELECT device_id, sequence, envelope_json, applied_at FROM batches \
         ORDER BY device_id ASC, sequence ASC",
    )
    .fetch_all(&mut *connection)
    .await?;
    let mut intervals = selected_coverage
        .iter()
        .flat_map(|(device_id, ranges)| {
            ranges.iter().map(|range| {
                (
                    device_id.clone(),
                    parse_sequence(&range.start).expect("validated checkpoint sequence"),
                    parse_sequence(&range.end).expect("validated checkpoint sequence"),
                )
            })
        })
        .collect::<Vec<_>>();
    let mut tail_batches = 0_u64;
    let mut tail_payload_bytes = 0_u64;
    let mut created_at = selected_checkpoint_created_at(&mut *connection).await?;
    for row in rows {
        let device_id: String = row.try_get("device_id")?;
        let sequence: String = row.try_get("sequence")?;
        let sequence_number = parse_sequence(&sequence)?;
        intervals.push((device_id.clone(), sequence_number, sequence_number));
        let envelope_json: String = row.try_get("envelope_json")?;
        let envelope: UpdateEnvelope = serde_json::from_str(&envelope_json)?;
        created_at = Some(match created_at {
            Some(current) if current >= envelope.created_at => current,
            _ => envelope.created_at.clone(),
        });
        if !coverage_contains(&selected_coverage, &device_id, &sequence) {
            tail_batches = tail_batches
                .checked_add(1)
                .ok_or(StoreError::NumericRange("checkpoint tail batch count"))?;
            tail_payload_bytes = tail_payload_bytes
                .checked_add(decoded_base64_len(&envelope.payload)?)
                .ok_or(StoreError::NumericRange("checkpoint tail payload bytes"))?;
        }
    }
    if !force
        && tail_batches < CHECKPOINT_BATCH_THRESHOLD
        && tail_payload_bytes < CHECKPOINT_PAYLOAD_THRESHOLD
    {
        return Ok(None);
    }
    if intervals.is_empty() {
        return Ok(None);
    }

    let coverage = merge_coverage(intervals)?;
    let state = sqlx::query(
        "SELECT snapshot, snapshot_sha256 FROM canonical_state WHERE singleton = 1",
    )
    .fetch_one(&mut *connection)
    .await?;
    let snapshot: Vec<u8> = state.try_get("snapshot")?;
    let expected_snapshot_sha256: String = state.try_get("snapshot_sha256")?;
    if sha256_hex(&snapshot) != expected_snapshot_sha256 {
        return Err(StoreError::InvalidStore(
            "canonical snapshot checksum mismatch".into(),
        ));
    }
    let library_id: String =
        sqlx::query_scalar("SELECT value FROM store_meta WHERE key = 'library_id'")
            .fetch_one(&mut *connection)
            .await?;
    let artifact = create_checkpoint(
        &snapshot,
        &library_id,
        created_at.as_deref().unwrap_or("1970-01-01T00:00:00Z"),
        coverage,
    )?;
    persist_checkpoint(&mut *connection, &artifact, "local").await?;
    Ok(Some(PendingCheckpoint {
        path: artifact.path,
        checkpoint_id: artifact.checkpoint_id,
        checkpoint_json: artifact.json,
        batch_count: artifact.batch_count,
    }))
}

async fn persist_checkpoint(
    connection: &mut SqliteConnection,
    artifact: &CheckpointArtifact,
    origin: &str,
) -> StoreResult<()> {
    let coverage_json = serde_json::to_string(&artifact.coverage)?;
    sqlx::query(
        "INSERT INTO checkpoints \
         (path, checkpoint_id, checkpoint_json, batch_count, coverage_json, created_at, \
          origin, applied_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(path) DO NOTHING",
    )
    .bind(&artifact.path)
    .bind(&artifact.checkpoint_id)
    .bind(&artifact.json)
    .bind(
        i64::try_from(artifact.batch_count)
            .map_err(|_| StoreError::NumericRange("checkpoint batch count"))?,
    )
    .bind(&coverage_json)
    .bind(&artifact.created_at)
    .bind(origin)
    .bind(now_rfc3339())
    .execute(&mut *connection)
    .await?;
    let stored: String =
        sqlx::query_scalar("SELECT checkpoint_json FROM checkpoints WHERE path = ?")
            .bind(&artifact.path)
            .fetch_one(&mut *connection)
            .await?;
    if stored.as_bytes() != artifact.json.as_bytes() {
        return Err(StoreError::SyncIntegrity(
            "checkpoint identity collision".into(),
        ));
    }
    for (device_id, intervals) in &artifact.coverage {
        for interval in intervals {
            sqlx::query(
                "INSERT OR IGNORE INTO checkpoint_coverage \
                 (checkpoint_path, device_id, start_sequence, end_sequence) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&artifact.path)
            .bind(device_id)
            .bind(&interval.start)
            .bind(&interval.end)
            .execute(&mut *connection)
            .await?;
        }
    }
    Ok(())
}

async fn select_checkpoint(
    connection: &mut SqliteConnection,
    artifact: &CheckpointArtifact,
) -> StoreResult<()> {
    let selected_count: Option<i64> = sqlx::query_scalar(
        "SELECT c.batch_count FROM selected_checkpoint s \
         JOIN checkpoints c ON c.path = s.checkpoint_path WHERE s.singleton = 1",
    )
    .fetch_optional(&mut *connection)
    .await?;
    if selected_count.is_some_and(|count| count > artifact.batch_count as i64) {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO selected_checkpoint (singleton, checkpoint_path, selected_at) \
         VALUES (1, ?, ?) ON CONFLICT(singleton) DO UPDATE SET \
         checkpoint_path = excluded.checkpoint_path, selected_at = excluded.selected_at",
    )
    .bind(&artifact.path)
    .bind(now_rfc3339())
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// True when every operation this checkpoint covers has already been applied
/// here.
///
/// Selection only decides which operations a later pull may skip, so a
/// checkpoint whose coverage this replica already contains is safe to select
/// even when local state has moved past it. Requiring the canonical snapshot to
/// equal the checkpoint instead would leave a self-created checkpoint
/// unselected whenever a concurrent local edit landed while it uploaded. The
/// tail is measured from the selected checkpoint, so it would never reset and
/// every later sync would mint and upload another full snapshot.
async fn coverage_is_locally_applied(
    connection: &mut SqliteConnection,
    coverage: &CheckpointCoverage,
) -> StoreResult<bool> {
    let mut intervals = Vec::new();
    for (device_id, ranges) in selected_coverage(&mut *connection).await? {
        for range in &ranges {
            let (start, end) = interval_bounds(range)?;
            intervals.push((device_id.clone(), start, end));
        }
    }
    let rows = sqlx::query("SELECT device_id, sequence FROM batches")
        .fetch_all(&mut *connection)
        .await?;
    for row in rows {
        let device_id: String = row.try_get("device_id")?;
        let sequence: String = row.try_get("sequence")?;
        let sequence = parse_sequence(&sequence)?;
        intervals.push((device_id, sequence, sequence));
    }
    let applied = merge_coverage(intervals)?;
    for (device_id, ranges) in coverage {
        let Some(applied_ranges) = applied.get(device_id) else {
            return Ok(false);
        };
        let applied_bounds = applied_ranges
            .iter()
            .map(interval_bounds)
            .collect::<StoreResult<Vec<_>>>()?;
        for range in ranges {
            let (start, end) = interval_bounds(range)?;
            // Merged intervals never overlap, so a covered range that is
            // applied at all sits inside exactly one of them.
            if !applied_bounds.iter().any(|(applied_start, applied_end)| {
                *applied_start <= start && end <= *applied_end
            }) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn interval_bounds(interval: &CoverageInterval) -> StoreResult<(u64, u64)> {
    Ok((
        parse_sequence(&interval.start)?,
        parse_sequence(&interval.end)?,
    ))
}

async fn selected_coverage(
    connection: &mut SqliteConnection,
) -> StoreResult<CheckpointCoverage> {
    let rows = sqlx::query(
        "SELECT cc.device_id, cc.start_sequence, cc.end_sequence \
         FROM checkpoint_coverage cc JOIN selected_checkpoint s \
         ON s.checkpoint_path = cc.checkpoint_path WHERE s.singleton = 1 \
         ORDER BY cc.device_id ASC, cc.start_sequence ASC",
    )
    .fetch_all(&mut *connection)
    .await?;
    let mut coverage = BTreeMap::<String, Vec<CoverageInterval>>::new();
    for row in rows {
        coverage
            .entry(row.try_get("device_id")?)
            .or_default()
            .push(CoverageInterval {
                start: row.try_get("start_sequence")?,
                end: row.try_get("end_sequence")?,
            });
    }
    Ok(coverage)
}

async fn selected_checkpoint_created_at(
    connection: &mut SqliteConnection,
) -> StoreResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT c.created_at FROM checkpoints c JOIN selected_checkpoint s \
         ON s.checkpoint_path = c.path WHERE s.singleton = 1",
    )
    .fetch_optional(&mut *connection)
    .await?)
}

async fn checkpoint_covers<'e, E>(
    executor: E,
    device_id: &str,
    sequence: &str,
) -> StoreResult<bool>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM checkpoint_coverage cc JOIN selected_checkpoint s \
         ON s.checkpoint_path = cc.checkpoint_path WHERE s.singleton = 1 \
         AND cc.device_id = ? AND cc.start_sequence <= ? AND cc.end_sequence >= ?",
    )
    .bind(device_id)
    .bind(sequence)
    .bind(sequence)
    .fetch_one(executor)
    .await?;
    Ok(count > 0)
}

async fn store_is_pristine(connection: &mut SqliteConnection) -> StoreResult<bool> {
    let count: i64 = sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM batches) + (SELECT COUNT(*) FROM items) + \
         (SELECT COUNT(*) FROM import_rows) + (SELECT COUNT(*) FROM outbox)",
    )
    .fetch_one(&mut *connection)
    .await?;
    Ok(count == 0)
}

fn merge_coverage(mut intervals: Vec<(String, u64, u64)>) -> StoreResult<CheckpointCoverage> {
    intervals.sort();
    let mut merged = BTreeMap::<String, Vec<(u64, u64)>>::new();
    for (device_id, start, end) in intervals {
        let device = merged.entry(device_id).or_default();
        if let Some((_, previous_end)) = device.last_mut()
            && start <= previous_end.saturating_add(1)
        {
            *previous_end = (*previous_end).max(end);
            continue;
        }
        device.push((start, end));
    }
    merged
        .into_iter()
        .map(|(device_id, intervals)| {
            let intervals = intervals
                .into_iter()
                .map(|(start, end)| CoverageInterval::new(start, end).map_err(StoreError::from))
                .collect::<StoreResult<Vec<_>>>()?;
            Ok((device_id, intervals))
        })
        .collect()
}

fn parse_sequence(value: &str) -> StoreResult<u64> {
    value
        .parse::<u64>()
        .ok()
        .filter(|sequence| *sequence > 0 && value.len() == 20)
        .ok_or_else(|| StoreError::InvalidStore("invalid batch sequence".into()))
}

fn decoded_base64_len(value: &str) -> StoreResult<u64> {
    let bytes = STANDARD
        .decode(value)
        .map_err(|_| StoreError::InvalidStore("stored update payload is not Base64".into()))?;
    u64::try_from(bytes.len())
        .map_err(|_| StoreError::NumericRange("decoded update payload bytes"))
}

async fn observe_remote(
    connection: &mut SqliteConnection,
    path: &str,
    blob_sha: &str,
) -> StoreResult<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO remote_observations (path, blob_sha, observed_at) \
         VALUES (?, ?, ?)",
    )
    .bind(path)
    .bind(blob_sha)
    .bind(now_rfc3339())
    .execute(&mut *connection)
    .await?;
    let observed: String =
        sqlx::query_scalar("SELECT blob_sha FROM remote_observations WHERE path = ?")
            .bind(path)
            .fetch_one(&mut *connection)
            .await?;
    if observed != blob_sha {
        return Err(StoreError::SyncIntegrity(format!(
            "immutable remote path {path} changed after it was observed"
        )));
    }
    Ok(())
}

async fn remove_outbox(
    connection: &mut SqliteConnection,
    envelope: &UpdateEnvelope,
) -> StoreResult<bool> {
    let result = sqlx::query("DELETE FROM outbox WHERE device_id = ? AND sequence = ?")
        .bind(&envelope.device_id)
        .bind(&envelope.sequence)
        .execute(&mut *connection)
        .await?;
    Ok(result.rows_affected() == 1)
}

fn validate_timestamp(value: &str, label: &str) -> StoreResult<()> {
    let parsed: DateTime<FixedOffset> = DateTime::parse_from_rfc3339(value)
        .map_err(|_| StoreError::SyncIntegrity(format!("invalid {label}")))?;
    if parsed.offset().local_minus_utc() != 0 {
        return Err(StoreError::SyncIntegrity(format!("{label} is not in UTC")));
    }
    Ok(())
}

fn validate_blob_sha(blob_sha: &str) -> StoreResult<()> {
    if !matches!(blob_sha.len(), 40 | 64)
        || !blob_sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(StoreError::SyncIntegrity(
            "remote blob has an invalid object ID".into(),
        ));
    }
    Ok(())
}

fn validate_error_kind(kind: &str) -> StoreResult<()> {
    if kind.is_empty()
        || kind.len() > 64
        || !kind
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        return Err(StoreError::InvalidInput(
            "sync error kind must be lowercase ASCII words".into(),
        ));
    }
    Ok(())
}
