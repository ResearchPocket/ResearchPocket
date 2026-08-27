//! Durable, local-only persistence for a V2 ResearchPocket library.

mod aggregate;
mod aggregate_sync;
mod captured;
mod enrichment;
mod error;
mod import;
mod model;
mod mutation;
mod store;
mod sync;
mod zen;

pub use aggregate::{AggregateGeneration, AggregateMigrationReceipt};
pub use aggregate_sync::{
    AggregateDisposition, PendingAggregateOperation, RemoteAggregateResult,
};
pub use enrichment::ENRICHMENT_MAX_ATTEMPTS;
pub use error::{StoreError, StoreResult};
pub use model::{
    CreateItemRequest, EditItemRequest, EnrichmentApplyResult, EnrichmentCandidates,
    EnrichmentClaim, EnrichmentJob, EnrichmentProvider, EnrichmentQueueCounts,
    EnrichmentStatus, ImportRejection, ImportResult, ListPage, ListQuery, ListResult,
    OptionalTextUpdate, PendingBatch, PendingCapturedDocument, PendingCheckpoint,
    RemoteBatchDisposition, RemoteBatchResult, RemoteCheckpointResult, RemotePackResult,
    SearchQuery, SearchResult, SourceBundleReceipt, SourceFileReceipt, StoreStatus, StoredItem,
    SyncConfiguration, SyncIdentity,
};
pub use store::V2Store;
pub use zen::{
    CreateZenDocumentRequest, EditZenDocumentRequest, ZenListQuery, zen_aggregate_kind,
};
