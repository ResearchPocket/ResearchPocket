use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, FixedOffset};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use reqwest::{Client, Response, StatusCode, Url};
use research_domain::{
    AGGREGATE_GENESIS_PATH, AGGREGATE_OPS_ROOT, AggregateCatalogue, AggregateGenesis,
    AggregateKind, DomainError, LibraryGenesis, MAX_OPERATION_PACK_BYTES,
    MAX_OPERATION_PACK_MEMBERS, OperationPackArtifact, aggregate_catalogue_path,
    create_operation_pack, validate_checkpoint,
};
use research_store::{
    AggregateDisposition, AggregateGeneration, PendingAggregateOperation, PendingBatch,
    PendingCapturedDocument, RemoteBatchDisposition, StoreError, SyncConfiguration, V2Store,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const API_ROOT: &str = "https://api.github.com/";
const API_VERSION: &str = "2026-03-10";
const GENESIS_PATH: &str = "sync/v1/library.json";
const OPS_PREFIX: &str = "sync/v1/ops/";
const PACKS_PREFIX: &str = "sync/v1/ops/packs/";
const CHECKPOINTS_PREFIX: &str = "sync/v1/checkpoints/";
const MAX_UPLOAD_ATTEMPTS: usize = 4;
const PACK_JSON_OVERHEAD_ALLOWANCE: usize = 1_024;

#[derive(Debug, Error)]
pub enum SyncError {
    #[error(
        "GitHub credential is missing; set RESEARCHPOCKET_GITHUB_TOKEN or GH_TOKEN for this process"
    )]
    MissingCredential,
    #[error("GitHub credential contains characters that cannot be used in an HTTP header")]
    InvalidCredential,
    #[error("repository must be written as OWNER/NAME")]
    InvalidRepository,
    #[error("the configured GitHub repository must be private")]
    PublicRepository,
    #[error("the configured GitHub repository is archived or disabled")]
    UnavailableRepository,
    #[error("the selected branch does not exist in the GitHub repository")]
    MissingBranch,
    #[error("this library is already connected to another synchronization remote")]
    AlreadyConfigured,
    #[error("GitHub transport failed before a usable response was received")]
    Transport(#[source] reqwest::Error),
    #[error("GitHub API request failed with HTTP {status} ({kind})")]
    Api {
        status: u16,
        kind: &'static str,
        retry_after_seconds: Option<u64>,
    },
    #[error("remote synchronization data is malformed: {0}")]
    RemoteData(String),
    #[error("remote synchronization integrity failure: {0}")]
    Integrity(String),
    #[error("synchronization remained contended after bounded retries")]
    Contention,
    #[error("local synchronization store failed: {0}")]
    Store(#[from] StoreError),
    #[error("synchronization domain validation failed: {0}")]
    Domain(#[from] DomainError),
    #[error("local synchronization JSON encoding failed: {0}")]
    Json(#[from] serde_json::Error),
}

impl SyncError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::MissingCredential | Self::InvalidCredential => "authentication",
            Self::InvalidRepository | Self::MissingBranch | Self::AlreadyConfigured => {
                "configuration"
            }
            Self::PublicRepository | Self::UnavailableRepository => "repository_policy",
            Self::Transport(_) => "transport",
            Self::Api { kind, .. } => kind,
            Self::RemoteData(_) | Self::Integrity(_) => "integrity",
            Self::Contention => "contention",
            Self::Store(error) => store_error_kind(error),
            Self::Domain(error) => domain_error_kind(error),
            Self::Json(_) => "integrity",
        }
    }

    pub fn is_retryable(&self) -> bool {
        matches!(
            self.kind(),
            "transport" | "rate_limited" | "server" | "contention"
        )
    }

    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Api {
                retry_after_seconds: Some(seconds),
                ..
            } => Some(Duration::from_secs(*seconds)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SyncRemote {
    pub owner: String,
    pub repository: String,
    pub branch: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SyncCycleResult {
    pub remote: SyncRemote,
    pub remote_batches_seen: u64,
    pub downloaded: u64,
    pub applied: u64,
    pub already_applied: u64,
    pub acknowledged: u64,
    pub uploaded: u64,
    pub pending: u64,
    /// Aggregate-scoped work — zen documents today, items once they move.
    /// Counted apart from v1 because the two generations queue separately and
    /// a number that mixed them could not be reconciled with either queue.
    pub aggregates_uploaded: u64,
    pub aggregates_applied: u64,
    pub aggregates_pending: u64,
}

#[derive(Default)]
struct AggregateStats {
    uploaded: u64,
    applied: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SyncConnectResult {
    pub remote: SyncRemote,
    pub adopted_remote_library: bool,
    pub cycle: SyncCycleResult,
}

struct GitHubClient {
    http: Client,
}

#[derive(Clone)]
struct Remote {
    owner: String,
    repository: String,
    branch: String,
}

#[derive(Deserialize)]
struct RepositoryResponse {
    private: bool,
    archived: bool,
    disabled: bool,
    default_branch: String,
    size: u64,
}

struct RepositoryInfo {
    default_branch: String,
    empty: bool,
}

#[derive(Default)]
struct ProtocolTree {
    blobs: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct TreeResponse {
    sha: String,
    tree: Vec<TreeEntry>,
    truncated: bool,
}

#[derive(Deserialize)]
struct TreeEntry {
    path: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}

#[derive(Deserialize)]
struct BlobResponse {
    content: String,
    encoding: String,
    sha: String,
    size: u64,
}

#[derive(Serialize)]
struct PutContent<'a> {
    message: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<&'a str>,
}

#[derive(Deserialize)]
struct PutContentResponse {
    content: Option<PutBlob>,
}

#[derive(Deserialize)]
struct PutBlob {
    sha: String,
}

enum PutResult {
    Created(String),
    Race,
    Ambiguous(&'static str),
}

enum PendingUpload {
    Direct(PendingBatch),
    Pack {
        artifact: OperationPackArtifact,
        members: Vec<PendingBatch>,
    },
}

impl PendingUpload {
    fn path(&self) -> &str {
        match self {
            Self::Direct(batch) => &batch.path,
            Self::Pack { artifact, .. } => &artifact.path,
        }
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Self::Direct(batch) => batch.envelope_json.as_bytes(),
            Self::Pack { artifact, .. } => artifact.json.as_bytes(),
        }
    }

    fn member_paths(&self) -> Vec<String> {
        match self {
            Self::Direct(batch) => vec![batch.path.clone()],
            Self::Pack { members, .. } => {
                members.iter().map(|batch| batch.path.clone()).collect()
            }
        }
    }

    fn logical_count(&self) -> Result<u64, SyncError> {
        let count = match self {
            Self::Direct(_) => 1,
            Self::Pack { members, .. } => members.len(),
        };
        u64::try_from(count)
            .map_err(|_| StoreError::NumericRange("pending upload member count").into())
    }

    async fn confirm(
        &self,
        store: &V2Store,
        blob_sha: &str,
        bytes: &[u8],
    ) -> Result<PullStats, SyncError> {
        let mut stats = PullStats::default();
        match self {
            Self::Direct(batch) => {
                let result = store
                    .receive_remote_batch(&batch.path, blob_sha, bytes)
                    .await?;
                stats.downloaded = 1;
                match result.disposition {
                    RemoteBatchDisposition::Applied => stats.applied = 1,
                    RemoteBatchDisposition::AlreadyApplied => stats.already_applied = 1,
                }
                if result.acknowledged_outbox {
                    stats.acknowledged = 1;
                }
            }
            Self::Pack { artifact, .. } => {
                let result = store
                    .receive_remote_pack(&artifact.path, blob_sha, bytes)
                    .await?;
                stats.downloaded = result.member_count;
                stats.applied = result.applied;
                stats.already_applied = result.already_applied;
                stats.acknowledged = result.acknowledged_outbox;
            }
        }
        Ok(stats)
    }
}

#[derive(Default)]
struct PullStats {
    seen: u64,
    downloaded: u64,
    applied: u64,
    already_applied: u64,
    acknowledged: u64,
}

impl PullStats {
    fn add(&mut self, other: Self) {
        self.seen += other.seen;
        self.downloaded += other.downloaded;
        self.applied += other.applied;
        self.already_applied += other.already_applied;
        self.acknowledged += other.acknowledged;
    }
}

pub async fn connect(
    store: &V2Store,
    repository: &str,
    requested_branch: Option<&str>,
) -> Result<SyncConnectResult, SyncError> {
    let (owner, repository) = parse_repository(repository)?;
    let existing = store.sync_configuration().await?;
    if existing.as_ref().is_some_and(|configuration| {
        configuration.owner != owner || configuration.repository != repository
    }) {
        return Err(SyncError::AlreadyConfigured);
    }
    let client = GitHubClient::from_environment()?;
    let repository_info = client.inspect_repository(&owner, &repository).await?;
    let branch = requested_branch
        .map(str::to_owned)
        .or_else(|| {
            existing
                .as_ref()
                .map(|configuration| configuration.branch.clone())
        })
        .unwrap_or_else(|| repository_info.default_branch.clone());
    if branch.trim().is_empty() {
        return Err(SyncError::MissingBranch);
    }
    if repository_info.empty && branch != repository_info.default_branch {
        return Err(SyncError::MissingBranch);
    }
    if existing
        .as_ref()
        .is_some_and(|configuration| configuration.branch != branch)
    {
        return Err(SyncError::AlreadyConfigured);
    }
    let remote = Remote {
        owner,
        repository,
        branch,
    };
    let (genesis, adopted_remote_library) =
        connect_genesis(store, &client, &remote, repository_info.empty).await?;
    genesis.validate()?;
    validate_utc(&genesis.created_at, "genesis creation time")?;
    store
        .configure_sync(&remote.owner, &remote.repository, &remote.branch)
        .await?;

    let cycle = run_configured(store, &client, &remote).await;
    match cycle {
        Ok(cycle) => {
            store.record_sync_success().await?;
            Ok(SyncConnectResult {
                remote: remote.view(),
                adopted_remote_library,
                cycle,
            })
        }
        Err(error) => {
            let _ = store.record_sync_failure(error.kind()).await;
            Err(error)
        }
    }
}

pub async fn run_once(store: &V2Store) -> Result<SyncCycleResult, SyncError> {
    let configuration = store
        .sync_configuration()
        .await?
        .ok_or(StoreError::SyncNotConfigured)?;
    let remote = Remote::from_configuration(&configuration);
    let result = async {
        let client = GitHubClient::from_environment()?;
        run_configured(store, &client, &remote).await
    }
    .await;
    match result {
        Ok(result) => {
            store.record_sync_success().await?;
            Ok(result)
        }
        Err(error) => {
            let _ = store.record_sync_failure(error.kind()).await;
            Err(error)
        }
    }
}

async fn connect_genesis(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
    repository_empty: bool,
) -> Result<(LibraryGenesis, bool), SyncError> {
    let mut empty_bootstrap = repository_empty;
    let local_genesis = store.sync_genesis().await?;
    let local_genesis_bytes = serde_json::to_vec(&local_genesis)?;
    for attempt in 0..MAX_UPLOAD_ATTEMPTS {
        let tree = if empty_bootstrap {
            ProtocolTree::default()
        } else {
            client.discover(remote).await?
        };
        if let Some(sha) = tree.blobs.get(GENESIS_PATH) {
            let bytes = client.download_blob(remote, sha).await?;
            let genesis = parse_genesis(&bytes)?;
            let identity = store.sync_identity().await?;
            let adopted = if identity.library_id == genesis.library_id {
                false
            } else {
                store
                    .adopt_library_id_if_pristine(&genesis.library_id)
                    .await?
            };
            return Ok((genesis, adopted));
        }
        if tree.blobs.keys().any(|path| {
            path.starts_with(OPS_PREFIX) || path.starts_with("sync/v1/checkpoints/")
        }) {
            return Err(SyncError::Integrity(
                "protocol operations exist without immutable library genesis".into(),
            ));
        }

        let branch = (!empty_bootstrap).then_some(remote.branch.as_str());
        match client
            .put_new(remote, GENESIS_PATH, &local_genesis_bytes, branch)
            .await?
        {
            PutResult::Created(_) => return Ok((local_genesis, false)),
            PutResult::Race | PutResult::Ambiguous(_) => {
                empty_bootstrap = false;
                retry_delay(GENESIS_PATH, attempt).await;
            }
        }
    }
    Err(SyncError::Contention)
}

async fn run_configured(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
) -> Result<SyncCycleResult, SyncError> {
    client
        .inspect_repository(&remote.owner, &remote.repository)
        .await?;
    let mut pull = pull_remote(store, client, remote).await?;
    ensure_captured_documents_uploaded(store, client, remote).await?;
    let pending = prepare_uploads(store.pending_batches().await?)?;
    let mut uploaded = 0_u64;
    for upload in pending {
        let (created, race_pull) = ensure_uploaded(store, client, remote, &upload).await?;
        if created {
            uploaded += upload.logical_count()?;
        }
        pull.add(race_pull);
    }
    pull.add(pull_remote(store, client, remote).await?);
    ensure_checkpoint_uploaded(store, client, remote).await?;
    let aggregates = run_aggregates(store, client, remote).await?;
    let pending = u64::try_from(store.pending_batches().await?.len())
        .map_err(|_| StoreError::NumericRange("pending sync batch count"))?;
    Ok(SyncCycleResult {
        remote: remote.view(),
        remote_batches_seen: pull.seen,
        downloaded: pull.downloaded,
        applied: pull.applied,
        already_applied: pull.already_applied,
        acknowledged: pull.acknowledged,
        uploaded,
        pending,
        aggregates_uploaded: aggregates.uploaded,
        aggregates_applied: aggregates.applied,
        aggregates_pending: store.pending_aggregate_operation_count().await?,
    })
}

/// Carries the aggregate generation: publish or adopt it, upload what is
/// queued, then apply what is there.
///
/// Runs after the v1 phase so the migration barrier is already on the remote by
/// the time any v2 object is: an older client meets the barrier and stops
/// before it can encounter a namespace it does not implement.
async fn run_aggregates(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
) -> Result<AggregateStats, SyncError> {
    reconcile_generation(store, client, remote).await?;
    let mut stats = AggregateStats::default();
    for operation in store.pending_aggregate_operations().await? {
        if ensure_aggregate_uploaded(store, client, remote, &operation).await? {
            stats.uploaded += 1;
        }
    }
    stats.applied = apply_remote_aggregates(store, client, remote).await?;
    Ok(stats)
}

/// Agrees with the remote on which v2 generation this library is in.
///
/// Genesis is immutable, so there is exactly one right answer per library: the
/// first device to publish defines it and everyone else adopts. A device that
/// finds a genesis it cannot reconcile with its own stops rather than writing
/// into a history it does not share.
async fn reconcile_generation(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
) -> Result<(), SyncError> {
    let tree = client.discover(remote).await?;
    let Some(sha) = tree.blobs.get(AGGREGATE_GENESIS_PATH) else {
        if let Some(generation) = store.aggregate_generation().await? {
            publish_generation(store, client, remote, &generation).await?;
        }
        return Ok(());
    };

    let genesis_json = remote_text(client, remote, sha, AGGREGATE_GENESIS_PATH).await?;
    let genesis: AggregateGenesis = serde_json::from_str(&genesis_json)
        .map_err(|_| SyncError::RemoteData("aggregate genesis JSON is invalid".into()))?;
    let catalogue_path = aggregate_catalogue_path(&genesis.catalogue_sha256);
    let catalogue_sha = tree.blobs.get(&catalogue_path).ok_or_else(|| {
        SyncError::Integrity(
            "aggregate genesis names a catalogue the repository does not contain".into(),
        )
    })?;
    let catalogue_json = remote_text(client, remote, catalogue_sha, &catalogue_path).await?;
    // Adoption revalidates both artifacts against this library and fails closed
    // on an aggregate kind this build does not implement.
    store
        .adopt_aggregate_generation(&genesis_json, &catalogue_json)
        .await?;
    store
        .record_immutable_remote_blob(AGGREGATE_GENESIS_PATH, sha)
        .await?;

    let catalogue: AggregateCatalogue = serde_json::from_str(&catalogue_json)
        .map_err(|_| SyncError::RemoteData("aggregate catalogue JSON is invalid".into()))?;
    seed_missing_replicas(store, client, remote, &tree, &catalogue).await?;
    publish_missing_checkpoints(store, client, remote, &tree).await
}

/// Downloads the checkpoints for catalogue entries this device has no replica
/// of, which is how a device that did not migrate joins the generation.
async fn seed_missing_replicas(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
    tree: &ProtocolTree,
    catalogue: &AggregateCatalogue,
) -> Result<(), SyncError> {
    let kinds = catalogue
        .entries
        .iter()
        .map(|entry| entry.aggregate_kind.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let mut present = BTreeSet::new();
    for kind in kinds {
        for id in store
            .known_aggregate_ids(&AggregateKind::from(kind.clone()))
            .await?
        {
            present.insert((kind.clone(), id));
        }
    }
    for entry in &catalogue.entries {
        let key = (
            entry.aggregate_kind.as_str().to_owned(),
            entry.aggregate_id.clone(),
        );
        if present.contains(&key) {
            continue;
        }
        let sha = tree.blobs.get(&entry.checkpoint_path).ok_or_else(|| {
            SyncError::Integrity(
                "the aggregate catalogue names a checkpoint the repository does not contain"
                    .into(),
            )
        })?;
        let json = remote_text(client, remote, sha, &entry.checkpoint_path).await?;
        store
            .receive_aggregate_checkpoint(&entry.checkpoint_path, &json)
            .await?;
        store
            .record_immutable_remote_blob(&entry.checkpoint_path, sha)
            .await?;
    }
    Ok(())
}

/// Publishes this device's generation. The catalogue goes up before genesis, so
/// genesis never names an artifact a reader cannot fetch.
async fn publish_generation(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
    generation: &AggregateGeneration,
) -> Result<(), SyncError> {
    put_immutable(
        client,
        remote,
        &generation.catalogue_path,
        generation.catalogue_json.as_bytes(),
    )
    .await?;
    put_immutable(
        client,
        remote,
        AGGREGATE_GENESIS_PATH,
        generation.genesis_json.as_bytes(),
    )
    .await?;
    let tree = client.discover(remote).await?;
    publish_missing_checkpoints(store, client, remote, &tree).await
}

async fn publish_missing_checkpoints(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
    tree: &ProtocolTree,
) -> Result<(), SyncError> {
    for (path, checkpoint_json) in store.aggregate_checkpoints().await? {
        if tree.blobs.contains_key(&path) {
            continue;
        }
        put_immutable(client, remote, &path, checkpoint_json.as_bytes()).await?;
    }
    Ok(())
}

/// Writes a content-addressed object, treating "already there" as success.
///
/// Every v2 artifact is named by the hash of its own bytes, so a losing race is
/// a race with an identical object.
async fn put_immutable(
    client: &GitHubClient,
    remote: &Remote,
    path: &str,
    bytes: &[u8],
) -> Result<(), SyncError> {
    for attempt in 0..MAX_UPLOAD_ATTEMPTS {
        match client
            .put_new(remote, path, bytes, Some(&remote.branch))
            .await?
        {
            PutResult::Created(_) | PutResult::Race => return Ok(()),
            PutResult::Ambiguous(_) => retry_delay(path, attempt).await,
        }
    }
    Err(SyncError::Contention)
}

async fn ensure_aggregate_uploaded(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
    operation: &PendingAggregateOperation,
) -> Result<bool, SyncError> {
    let bytes = operation.envelope_json.as_bytes();
    for attempt in 0..MAX_UPLOAD_ATTEMPTS {
        match client
            .put_new(remote, &operation.path, bytes, Some(&remote.branch))
            .await
        {
            Ok(PutResult::Created(sha)) => {
                // Confirming from the write is what clears the outbox, so a
                // crash before the next pull cannot leave the operation queued
                // forever.
                store
                    .receive_remote_aggregate_operation(&operation.path, &sha, bytes)
                    .await?;
                return Ok(true);
            }
            Ok(PutResult::Race) => {
                // The path already exists. Confirm from whatever is actually
                // there rather than assuming our bytes are what a reader sees.
                let tree = client.discover(remote).await?;
                if let Some(sha) = tree.blobs.get(&operation.path) {
                    let existing = client.download_blob(remote, sha).await?;
                    store
                        .receive_remote_aggregate_operation(&operation.path, sha, &existing)
                        .await?;
                    return Ok(false);
                }
                store
                    .record_aggregate_outbox_attempt(&operation.path, Some("contention"))
                    .await?;
                retry_delay(&operation.path, attempt).await;
            }
            Ok(PutResult::Ambiguous(kind)) => {
                store
                    .record_aggregate_outbox_attempt(&operation.path, Some(kind))
                    .await?;
                retry_delay(&operation.path, attempt).await;
            }
            Err(error) => {
                let _ = store
                    .record_aggregate_outbox_attempt(&operation.path, Some(error.kind()))
                    .await;
                return Err(error);
            }
        }
    }
    Err(SyncError::Contention)
}

/// Applies every remote aggregate operation this device has not seen.
///
/// Operations are retried in rounds rather than applied once each: one can
/// depend on another that sorts after it by path. A round that applies nothing
/// while work remains means a dependency is genuinely absent from the remote,
/// which is a broken history rather than a slow one.
async fn apply_remote_aggregates(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
) -> Result<u64, SyncError> {
    let tree = client.discover(remote).await?;
    let mut queue = Vec::new();
    for (path, sha) in tree
        .blobs
        .iter()
        .filter(|(path, _)| path.starts_with(AGGREGATE_OPS_ROOT))
    {
        if let Some(observed) = store.observed_remote_blob(path).await? {
            if observed == *sha {
                continue;
            }
            return Err(SyncError::Integrity(
                "an immutable aggregate operation changed after it was observed".into(),
            ));
        }
        let bytes = client.download_blob(remote, sha).await?;
        queue.push((path.clone(), sha.clone(), bytes));
    }

    let mut applied = 0_u64;
    while !queue.is_empty() {
        let mut deferred = Vec::new();
        let mut progressed = false;
        for (path, sha, bytes) in queue {
            let result = store
                .receive_remote_aggregate_operation(&path, &sha, &bytes)
                .await?;
            match result.disposition {
                AggregateDisposition::Deferred => deferred.push((path, sha, bytes)),
                AggregateDisposition::Applied => {
                    applied += 1;
                    progressed = true;
                }
                AggregateDisposition::AlreadyApplied => progressed = true,
            }
        }
        if !deferred.is_empty() && !progressed {
            return Err(SyncError::Integrity(
                "remote aggregate operations have missing causal dependencies".into(),
            ));
        }
        queue = deferred;
    }
    Ok(applied)
}

async fn remote_text(
    client: &GitHubClient,
    remote: &Remote,
    sha: &str,
    path: &str,
) -> Result<String, SyncError> {
    let bytes = client.download_blob(remote, sha).await?;
    String::from_utf8(bytes)
        .map_err(|_| SyncError::RemoteData(format!("{path} is not UTF-8 JSON")))
}

async fn pull_remote(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
) -> Result<PullStats, SyncError> {
    let tree = client.discover(remote).await?;
    validate_remote_genesis(store, client, remote, &tree).await?;
    maybe_restore_checkpoint(store, client, remote, &tree).await?;
    let operations = tree
        .blobs
        .iter()
        .filter(|(path, _)| path.starts_with(OPS_PREFIX))
        .collect::<Vec<_>>();
    let mut stats = PullStats::default();
    for (path, sha) in operations {
        stats.seen += 1;
        if store.remote_blob_is_current(path, sha).await? {
            continue;
        }
        let bytes = client.download_blob(remote, sha).await?;
        if path.starts_with(PACKS_PREFIX) {
            let result = store.receive_remote_pack(path, sha, &bytes).await?;
            stats.downloaded += 1;
            stats.applied += result.applied;
            stats.already_applied += result.already_applied;
            stats.acknowledged += result.acknowledged_outbox;
        } else {
            let result = store.receive_remote_batch(path, sha, &bytes).await?;
            stats.downloaded += 1;
            match result.disposition {
                RemoteBatchDisposition::Applied => stats.applied += 1,
                RemoteBatchDisposition::AlreadyApplied => stats.already_applied += 1,
            }
            if result.acknowledged_outbox {
                stats.acknowledged += 1;
            }
        }
    }
    if store.retry_deferred_batches().await? > 0 {
        return Err(SyncError::Integrity(
            "remote operations have missing causal dependencies".into(),
        ));
    }
    Ok(stats)
}

async fn maybe_restore_checkpoint(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
    tree: &ProtocolTree,
) -> Result<(), SyncError> {
    let identity = store.sync_identity().await?;
    if !identity.pristine {
        return Ok(());
    }
    let mut candidates = Vec::new();
    for (path, sha) in tree
        .blobs
        .iter()
        .filter(|(path, _)| path.starts_with(CHECKPOINTS_PREFIX))
    {
        let bytes = client.download_blob(remote, sha).await?;
        let json = std::str::from_utf8(&bytes)
            .map_err(|_| SyncError::RemoteData("checkpoint is not UTF-8 JSON".into()))?;
        let artifact = validate_checkpoint(path, json, &identity.library_id)?;
        candidates.push((
            artifact.batch_count,
            artifact.checkpoint_id,
            path,
            sha,
            bytes,
        ));
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    if let Some((_, _, path, sha, bytes)) = candidates.into_iter().next() {
        store.receive_remote_checkpoint(path, sha, &bytes).await?;
    }
    Ok(())
}

async fn ensure_checkpoint_uploaded(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
) -> Result<(), SyncError> {
    let Some(checkpoint) = store.checkpoint_candidate(false).await? else {
        return Ok(());
    };
    for attempt in 0..MAX_UPLOAD_ATTEMPTS {
        let tree = client.discover(remote).await?;
        if let Some(sha) = tree.blobs.get(&checkpoint.path) {
            let bytes = client.download_blob(remote, sha).await?;
            store
                .receive_remote_checkpoint(&checkpoint.path, sha, &bytes)
                .await?;
            return Ok(());
        }
        match client
            .put_new(
                remote,
                &checkpoint.path,
                checkpoint.checkpoint_json.as_bytes(),
                Some(&remote.branch),
            )
            .await?
        {
            PutResult::Created(sha) => {
                store
                    .receive_remote_checkpoint(
                        &checkpoint.path,
                        &sha,
                        checkpoint.checkpoint_json.as_bytes(),
                    )
                    .await?;
                return Ok(());
            }
            PutResult::Race | PutResult::Ambiguous(_) => {
                retry_delay(&checkpoint.path, attempt).await;
            }
        }
    }
    Err(SyncError::Contention)
}

async fn ensure_captured_documents_uploaded(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
) -> Result<(), SyncError> {
    for document in store.pending_captured_documents().await? {
        ensure_captured_document_uploaded(store, client, remote, &document).await?;
    }
    Ok(())
}

async fn ensure_captured_document_uploaded(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
    document: &PendingCapturedDocument,
) -> Result<(), SyncError> {
    for attempt in 0..MAX_UPLOAD_ATTEMPTS {
        let tree = client.discover(remote).await?;
        if let Some(sha) = tree.blobs.get(&document.path) {
            let bytes = client.download_blob(remote, sha).await?;
            store
                .confirm_captured_document(&document.reference, &document.path, sha, &bytes)
                .await?;
            return Ok(());
        }
        match client
            .put_new(
                remote,
                &document.path,
                document.markdown.as_bytes(),
                Some(&remote.branch),
            )
            .await?
        {
            PutResult::Created(sha) => {
                store
                    .confirm_captured_document(
                        &document.reference,
                        &document.path,
                        &sha,
                        document.markdown.as_bytes(),
                    )
                    .await?;
                return Ok(());
            }
            PutResult::Race | PutResult::Ambiguous(_) => {
                retry_delay(&document.path, attempt).await;
            }
        }
    }
    Err(SyncError::Contention)
}

async fn validate_remote_genesis(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
    tree: &ProtocolTree,
) -> Result<(), SyncError> {
    let sha = tree.blobs.get(GENESIS_PATH).ok_or_else(|| {
        SyncError::Integrity("the configured repository has no library genesis".into())
    })?;
    if let Some(observed) = store.observed_remote_blob(GENESIS_PATH).await?
        && observed != *sha
    {
        return Err(SyncError::Integrity(
            "the immutable library genesis changed after it was observed".into(),
        ));
    }
    let genesis = parse_genesis(&client.download_blob(remote, sha).await?)?;
    let local = store.sync_identity().await?;
    if genesis.library_id != local.library_id {
        return Err(SyncError::Integrity(format!(
            "remote library {} does not match this local library",
            genesis.library_id
        )));
    }
    store
        .record_immutable_remote_blob(GENESIS_PATH, sha)
        .await?;
    Ok(())
}

async fn ensure_uploaded(
    store: &V2Store,
    client: &GitHubClient,
    remote: &Remote,
    upload: &PendingUpload,
) -> Result<(bool, PullStats), SyncError> {
    let mut race_pull = PullStats::default();
    let member_paths = upload.member_paths();
    for attempt in 0..MAX_UPLOAD_ATTEMPTS {
        let tree = client.discover(remote).await?;
        if let Some(sha) = tree.blobs.get(upload.path()) {
            if !store.remote_blob_is_current(upload.path(), sha).await? {
                let bytes = client.download_blob(remote, sha).await?;
                race_pull.add(upload.confirm(store, sha, &bytes).await?);
            }
            return Ok((false, race_pull));
        }

        let put = client
            .put_new(remote, upload.path(), upload.bytes(), Some(&remote.branch))
            .await;
        match put {
            Ok(PutResult::Created(sha)) => {
                store.record_outbox_attempts(&member_paths, None).await?;
                race_pull.add(upload.confirm(store, &sha, upload.bytes()).await?);
                return Ok((true, race_pull));
            }
            Ok(PutResult::Race) => {
                store
                    .record_outbox_attempts(&member_paths, Some("contention"))
                    .await?;
                race_pull.add(pull_remote(store, client, remote).await?);
                retry_delay(upload.path(), attempt).await;
            }
            Ok(PutResult::Ambiguous(kind)) => {
                store
                    .record_outbox_attempts(&member_paths, Some(kind))
                    .await?;
                race_pull.add(pull_remote(store, client, remote).await?);
                retry_delay(upload.path(), attempt).await;
            }
            Err(error) => {
                let _ = store
                    .record_outbox_attempts(&member_paths, Some(error.kind()))
                    .await;
                return Err(error);
            }
        }
    }
    Err(SyncError::Contention)
}

fn prepare_uploads(pending: Vec<PendingBatch>) -> Result<Vec<PendingUpload>, SyncError> {
    let mut uploads = Vec::new();
    let mut current = Vec::<PendingBatch>::new();
    let mut current_estimated_bytes = PACK_JSON_OVERHEAD_ALLOWANCE;
    let maximum_estimated_bytes =
        MAX_OPERATION_PACK_BYTES.saturating_sub(PACK_JSON_OVERHEAD_ALLOWANCE);

    for batch in pending {
        let encoded_bytes = batch.envelope_json.len().div_ceil(3) * 4 + 3;
        let changes_device = current
            .first()
            .is_some_and(|first| first.device_id != batch.device_id);
        let exceeds_members = current.len() == MAX_OPERATION_PACK_MEMBERS;
        let exceeds_bytes = !current.is_empty()
            && current_estimated_bytes.saturating_add(encoded_bytes) > maximum_estimated_bytes;
        if changes_device || exceeds_members || exceeds_bytes {
            uploads.push(build_pending_upload(std::mem::take(&mut current))?);
            current_estimated_bytes = PACK_JSON_OVERHEAD_ALLOWANCE;
        }
        current_estimated_bytes = current_estimated_bytes.saturating_add(encoded_bytes);
        current.push(batch);
    }
    if !current.is_empty() {
        uploads.push(build_pending_upload(current)?);
    }
    Ok(uploads)
}

fn build_pending_upload(mut members: Vec<PendingBatch>) -> Result<PendingUpload, SyncError> {
    if members.len() == 1 {
        return Ok(PendingUpload::Direct(members.remove(0)));
    }
    let envelopes = members
        .iter()
        .map(|batch| batch.envelope_json.clone())
        .collect::<Vec<_>>();
    let artifact = create_operation_pack(&envelopes)?;
    Ok(PendingUpload::Pack { artifact, members })
}

impl GitHubClient {
    fn from_environment() -> Result<Self, SyncError> {
        let token = env::var("RESEARCHPOCKET_GITHUB_TOKEN")
            .ok()
            .filter(|token| !token.trim().is_empty())
            .or_else(|| {
                env::var("GH_TOKEN")
                    .ok()
                    .filter(|token| !token.trim().is_empty())
            })
            .ok_or(SyncError::MissingCredential)?;
        let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| SyncError::InvalidCredential)?;
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization);
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static(API_VERSION),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(concat!("ResearchPocket/", env!("CARGO_PKG_VERSION"))),
        );
        let http = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(SyncError::Transport)?;
        Ok(Self { http })
    }

    async fn inspect_repository(
        &self,
        owner: &str,
        repository: &str,
    ) -> Result<RepositoryInfo, SyncError> {
        let remote = Remote {
            owner: owner.to_owned(),
            repository: repository.to_owned(),
            branch: String::new(),
        };
        let response: RepositoryResponse = self.get_json(repo_url(&remote, &[])).await?;
        if !response.private {
            return Err(SyncError::PublicRepository);
        }
        if response.archived || response.disabled {
            return Err(SyncError::UnavailableRepository);
        }
        Ok(RepositoryInfo {
            default_branch: response.default_branch,
            empty: response.size == 0,
        })
    }

    async fn discover(&self, remote: &Remote) -> Result<ProtocolTree, SyncError> {
        let recursive = self.fetch_tree(remote, &remote.branch, true).await;
        let recursive = match recursive {
            Err(SyncError::Api { status: 404, .. }) => return Err(SyncError::MissingBranch),
            other => other?,
        };
        if !recursive.truncated {
            return collect_protocol_entries(recursive.tree, "");
        }

        let mut protocol = ProtocolTree::default();
        let mut stack = vec![(recursive.sha, String::new())];
        while let Some((tree_sha, prefix)) = stack.pop() {
            let tree = self.fetch_tree(remote, &tree_sha, false).await?;
            for entry in tree.tree {
                let path = join_path(&prefix, &entry.path);
                validate_reserved_directory(&path, &entry)?;
                if entry.kind == "tree" && protocol_tree_relevant(&path) {
                    stack.push((entry.sha, path));
                } else if is_protocol_path(&path) {
                    insert_protocol_blob(&mut protocol, &path, &entry)?;
                }
            }
        }
        Ok(protocol)
    }

    async fn fetch_tree(
        &self,
        remote: &Remote,
        tree_sha: &str,
        recursive: bool,
    ) -> Result<TreeResponse, SyncError> {
        let mut url = repo_url(remote, &["git", "trees", tree_sha]);
        if recursive {
            url.query_pairs_mut().append_pair("recursive", "1");
        }
        self.get_json(url).await
    }

    async fn download_blob(&self, remote: &Remote, sha: &str) -> Result<Vec<u8>, SyncError> {
        validate_git_sha(sha)?;
        let blob: BlobResponse = self
            .get_json(repo_url(remote, &["git", "blobs", sha]))
            .await?;
        if blob.sha != sha || blob.encoding != "base64" {
            return Err(SyncError::Integrity(
                "GitHub returned a blob with mismatched identity or encoding".into(),
            ));
        }
        let compact = blob
            .content
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect::<Vec<_>>();
        let bytes = STANDARD
            .decode(compact)
            .map_err(|_| SyncError::RemoteData("GitHub blob Base64 is invalid".into()))?;
        let length = u64::try_from(bytes.len())
            .map_err(|_| SyncError::RemoteData("GitHub blob is too large".into()))?;
        if length != blob.size {
            return Err(SyncError::Integrity(
                "GitHub blob size does not match its decoded content".into(),
            ));
        }
        Ok(bytes)
    }

    async fn put_new(
        &self,
        remote: &Remote,
        path: &str,
        bytes: &[u8],
        branch: Option<&str>,
    ) -> Result<PutResult, SyncError> {
        let body = PutContent {
            message: format!("researchpocket: append {path}"),
            content: STANDARD.encode(bytes),
            branch,
        };
        let response = match self
            .http
            .put(contents_url(remote, path))
            .json(&body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => return Ok(PutResult::Ambiguous("transport")),
        };
        match response.status() {
            StatusCode::CREATED => {
                let created: PutContentResponse =
                    response.json().await.map_err(SyncError::Transport)?;
                let sha = created
                    .content
                    .ok_or_else(|| {
                        SyncError::RemoteData(
                            "GitHub create response did not contain a blob identity".into(),
                        )
                    })?
                    .sha;
                validate_git_sha(&sha)?;
                Ok(PutResult::Created(sha))
            }
            StatusCode::CONFLICT | StatusCode::UNPROCESSABLE_ENTITY => Ok(PutResult::Race),
            status if status.is_server_error() => Ok(PutResult::Ambiguous("server")),
            StatusCode::OK => Err(SyncError::Integrity(
                "GitHub reported an update for an immutable create request".into(),
            )),
            _ => Err(api_error(&response)),
        }
    }

    async fn get_json<T: DeserializeOwned>(&self, url: Url) -> Result<T, SyncError> {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(SyncError::Transport)?;
        if !response.status().is_success() {
            return Err(api_error(&response));
        }
        response.json().await.map_err(SyncError::Transport)
    }
}

impl Remote {
    fn from_configuration(configuration: &SyncConfiguration) -> Self {
        Self {
            owner: configuration.owner.clone(),
            repository: configuration.repository.clone(),
            branch: configuration.branch.clone(),
        }
    }

    fn view(&self) -> SyncRemote {
        SyncRemote {
            owner: self.owner.clone(),
            repository: self.repository.clone(),
            branch: self.branch.clone(),
        }
    }
}

fn parse_repository(value: &str) -> Result<(String, String), SyncError> {
    let mut components = value.split('/');
    let owner = components.next().unwrap_or_default();
    let repository = components.next().unwrap_or_default();
    let safe = |part: &str| {
        !part.is_empty()
            && part.len() <= 100
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    if components.next().is_some() || !safe(owner) || !safe(repository) {
        return Err(SyncError::InvalidRepository);
    }
    Ok((owner.to_owned(), repository.to_owned()))
}

fn parse_genesis(bytes: &[u8]) -> Result<LibraryGenesis, SyncError> {
    let genesis: LibraryGenesis = serde_json::from_slice(bytes)
        .map_err(|_| SyncError::RemoteData("library genesis JSON is invalid".into()))?;
    genesis.validate()?;
    validate_utc(&genesis.created_at, "genesis creation time")?;
    Ok(genesis)
}

fn validate_utc(value: &str, label: &str) -> Result<(), SyncError> {
    let parsed: DateTime<FixedOffset> = DateTime::parse_from_rfc3339(value)
        .map_err(|_| SyncError::RemoteData(format!("{label} is not RFC 3339")))?;
    if parsed.offset().local_minus_utc() != 0 {
        return Err(SyncError::RemoteData(format!("{label} is not UTC")));
    }
    Ok(())
}

fn collect_protocol_entries(
    entries: Vec<TreeEntry>,
    prefix: &str,
) -> Result<ProtocolTree, SyncError> {
    let mut protocol = ProtocolTree::default();
    for entry in entries {
        let path = join_path(prefix, &entry.path);
        validate_reserved_directory(&path, &entry)?;
        if is_protocol_path(&path) && entry.kind != "tree" {
            insert_protocol_blob(&mut protocol, &path, &entry)?;
        }
    }
    Ok(protocol)
}

fn insert_protocol_blob(
    protocol: &mut ProtocolTree,
    path: &str,
    entry: &TreeEntry,
) -> Result<(), SyncError> {
    if entry.kind != "blob" || entry.mode != "100644" {
        return Err(SyncError::Integrity(
            "a protocol entry is not an ordinary file".into(),
        ));
    }
    validate_git_sha(&entry.sha)?;
    if protocol
        .blobs
        .insert(path.to_owned(), entry.sha.clone())
        .is_some()
    {
        return Err(SyncError::Integrity(
            "a protocol path appears more than once".into(),
        ));
    }
    Ok(())
}

fn validate_reserved_directory(path: &str, entry: &TreeEntry) -> Result<(), SyncError> {
    if matches!(
        path,
        "sync" | "sync/v1" | "sync/v2" | "sync/v2/content" | "sync/v2/content/sha256"
    ) && entry.kind != "tree"
    {
        return Err(SyncError::Integrity(
            "the reserved synchronization path is not a directory".into(),
        ));
    }
    Ok(())
}

/// Both generations live under `sync/`, and a client must see all of it:
/// discovering only what it writes would let it upload into a namespace it has
/// not read.
fn is_protocol_path(path: &str) -> bool {
    path.starts_with("sync/v1/") || path.starts_with("sync/v2/")
}

fn protocol_tree_relevant(path: &str) -> bool {
    path == "sync" || path == "sync/v1" || path == "sync/v2" || is_protocol_path(path)
}

fn join_path(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        path.to_owned()
    } else {
        format!("{prefix}/{path}")
    }
}

fn repo_url(remote: &Remote, suffix: &[&str]) -> Url {
    let mut url = Url::parse(API_ROOT).expect("static GitHub API URL must be valid");
    {
        let mut segments = url
            .path_segments_mut()
            .expect("GitHub API URL supports path segments");
        segments.pop_if_empty();
        segments.extend(["repos", &remote.owner, &remote.repository]);
        segments.extend(suffix.iter().copied());
    }
    url
}

fn contents_url(remote: &Remote, path: &str) -> Url {
    let mut url = repo_url(remote, &["contents"]);
    {
        let mut segments = url
            .path_segments_mut()
            .expect("GitHub API URL supports path segments");
        segments.extend(path.split('/'));
    }
    url
}

fn validate_git_sha(sha: &str) -> Result<(), SyncError> {
    if !matches!(sha.len(), 40 | 64)
        || !sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(SyncError::Integrity(
            "GitHub object identity is not lowercase hexadecimal".into(),
        ));
    }
    Ok(())
}

fn api_error(response: &Response) -> SyncError {
    let status = response.status();
    let rate_limited = status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN
            && response
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|value| value.to_str().ok())
                == Some("0"));
    let kind = if status == StatusCode::UNAUTHORIZED {
        "authentication"
    } else if rate_limited {
        "rate_limited"
    } else if status == StatusCode::FORBIDDEN {
        "authorization"
    } else if status == StatusCode::NOT_FOUND {
        "not_found"
    } else if status.is_server_error() {
        "server"
    } else {
        "github_api"
    };
    let retry_after_seconds = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .or_else(|| {
            response
                .headers()
                .get("x-ratelimit-reset")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .and_then(|reset| {
                    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
                    Some(reset.saturating_sub(now))
                })
        });
    SyncError::Api {
        status: status.as_u16(),
        kind,
        retry_after_seconds,
    }
}

async fn retry_delay(path: &str, attempt: usize) {
    let jitter = path.bytes().fold(0_u64, |state, byte| {
        state.wrapping_mul(33) ^ u64::from(byte)
    }) % 173;
    let exponent = u32::try_from(attempt).unwrap_or(u32::MAX).min(4);
    let base = 200_u64.saturating_mul(1_u64 << exponent);
    tokio::time::sleep(Duration::from_millis(base + jitter)).await;
}

fn store_error_kind(error: &StoreError) -> &'static str {
    match error {
        StoreError::SyncNotConfigured | StoreError::InvalidInput(_) => "configuration",
        StoreError::SyncLibraryMismatch(_) => "library_mismatch",
        StoreError::SyncIntegrity(_) | StoreError::Json(_) => "integrity",
        StoreError::Domain(error) => domain_error_kind(error),
        StoreError::Io(_) | StoreError::Sqlite(_) | StoreError::Migration(_) => "local_store",
        _ => "local_state",
    }
}

fn domain_error_kind(error: &DomainError) -> &'static str {
    match error {
        DomainError::UnsupportedProtocol(_)
        | DomainError::UnsupportedDomainSchema(_)
        | DomainError::UnsupportedCodec(_)
        | DomainError::UnsupportedFeature(_)
        | DomainError::UnsupportedOperationPackVersion(_) => "upgrade_required",
        _ => "integrity",
    }
}
