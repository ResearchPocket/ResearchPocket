use research_store::{
    AggregateDisposition, CreateZenDocumentRequest, EditZenDocumentRequest, StoreError,
    V2Store, ZenListQuery,
};

/// The workspace list must stay metadata-only and every edit must queue exactly
/// one aggregate-scoped operation.
#[tokio::test]
async fn zen_documents_project_metadata_and_queue_one_operation_per_edit() {
    let root = tempfile::tempdir().expect("temporary test root");
    let store = V2Store::init(root.path().join("library"))
        .await
        .expect("store");

    let created = store
        .create_zen_document(CreateZenDocumentRequest {
            title: Some("Today".into()),
            body: "- [x] Ship it\n- [ ] Review #132\n\nGrocery run.".into(),
            tags: vec!["reading".into()],
        })
        .await
        .expect("create document");
    assert_eq!((created.todo_total, created.todo_done), (2, 1));

    let listed = store.list_zen_documents().await.expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title.as_deref(), Some("Today"));
    assert_eq!(listed[0].tags, ["reading"]);
    assert_eq!(listed[0].byte_length, created.byte_length);

    let edited = store
        .edit_zen_document(EditZenDocumentRequest {
            document_id: created.document_id.clone(),
            body: Some("- [x] Ship it\n- [x] Review #132\n\nGrocery run.".into()),
            add_tags: vec!["crdt".into()],
            ..EditZenDocumentRequest::default()
        })
        .await
        .expect("edit document");
    assert_eq!(
        (edited.todo_total, edited.todo_done),
        (2, 2),
        "todo counts follow the body rather than a stored counter"
    );
    assert_eq!(edited.tags, ["crdt", "reading"]);

    let reopened = store
        .zen_document(&created.document_id)
        .await
        .expect("read document body");
    assert!(reopened.body.contains("- [x] Review #132"));

    // Deleting hides the document from the workspace without destroying it.
    store
        .delete_zen_document(&created.document_id)
        .await
        .expect("delete");
    assert!(store.list_zen_documents().await.expect("list").is_empty());
    assert_eq!(
        store
            .list_zen_documents_with(ZenListQuery {
                include_deleted: true
            })
            .await
            .expect("list")
            .len(),
        1,
        "a deleted document is still reachable by a view that asks for it"
    );
    store
        .restore_zen_document(&created.document_id)
        .await
        .expect("restore");
    assert_eq!(store.list_zen_documents().await.expect("list").len(), 1);

    // One operation per mutation: create, edit, delete, restore.
    let queued: i64 = sqlx_scalar(&store, "SELECT COUNT(*) FROM aggregate_outbox").await;
    assert_eq!(queued, 4);
    let items: i64 = sqlx_scalar(&store, "SELECT COUNT(*) FROM outbox").await;
    assert_eq!(items, 0, "zen work never lands in the protocol-v1 outbox");
}

/// A second device must reach the same document from the operations alone, and
/// must not lose an operation that arrives before the one it depends on.
#[tokio::test]
async fn zen_operations_converge_and_defer_until_their_dependency_lands() {
    let root = tempfile::tempdir().expect("temporary test root");
    let sender = V2Store::init(root.path().join("sender"))
        .await
        .expect("sender store");
    let receiver = V2Store::init(root.path().join("receiver"))
        .await
        .expect("receiver store");
    let identity = sender.sync_identity().await.expect("sender identity");
    receiver
        .adopt_library_id_if_pristine(&identity.library_id)
        .await
        .expect("adopt sender library");

    let created = sender
        .create_zen_document(CreateZenDocumentRequest {
            title: Some("CRDT reading log".into()),
            body: "- [ ] Read the paper".into(),
            tags: vec!["crdt".into()],
        })
        .await
        .expect("create document");
    sender
        .edit_zen_document(EditZenDocumentRequest {
            document_id: created.document_id.clone(),
            body: Some("- [x] Read the paper\n- [ ] Write it up".into()),
            ..EditZenDocumentRequest::default()
        })
        .await
        .expect("edit document");

    let queued = sender
        .pending_aggregate_operations()
        .await
        .expect("queued operations");
    assert_eq!(queued.len(), 2);

    // Deliberately backwards: the edit depends on the create, so it must be
    // declined rather than half-applied and recorded.
    let deferred = receiver
        .receive_remote_aggregate_operation(
            &queued[1].path,
            &"a".repeat(40),
            queued[1].envelope_json.as_bytes(),
        )
        .await
        .expect("receive the edit first");
    assert_eq!(deferred.disposition, AggregateDisposition::Deferred);
    assert!(
        receiver
            .list_zen_documents()
            .await
            .expect("list")
            .is_empty(),
        "a deferred operation must not create a document"
    );

    for operation in [&queued[0], &queued[1]] {
        let result = receiver
            .receive_remote_aggregate_operation(
                &operation.path,
                &"b".repeat(40),
                operation.envelope_json.as_bytes(),
            )
            .await
            .expect("receive in order");
        assert_eq!(result.disposition, AggregateDisposition::Applied);
    }

    assert_eq!(
        receiver.list_zen_documents().await.expect("receiver list"),
        sender.list_zen_documents().await.expect("sender list"),
    );
    let body = receiver
        .zen_document(&created.document_id)
        .await
        .expect("receiver body")
        .body;
    assert_eq!(
        body,
        sender
            .zen_document(&created.document_id)
            .await
            .expect("sender body")
            .body
    );
    assert!(body.contains("Write it up"));

    // The sender seeing its own operation come back is what clears the queue.
    let acknowledged = sender
        .receive_remote_aggregate_operation(
            &queued[0].path,
            &"c".repeat(40),
            queued[0].envelope_json.as_bytes(),
        )
        .await
        .expect("acknowledge own upload");
    assert_eq!(
        acknowledged.disposition,
        AggregateDisposition::AlreadyApplied
    );
    assert!(acknowledged.acknowledged_outbox);
    assert_eq!(
        sender
            .pending_aggregate_operation_count()
            .await
            .expect("remaining"),
        1
    );
}

async fn sqlx_scalar(store: &V2Store, query: &'static str) -> i64 {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!(
            "sqlite://{}",
            store.database_path().to_string_lossy()
        ))
        .await
        .expect("open database");
    sqlx::query_scalar(query)
        .fetch_one(&pool)
        .await
        .expect("scalar query")
}

/// A whole-body write carries the text it started from, so an editor buffer
/// cannot silently undo an edit that landed while it was open.
#[tokio::test]
async fn a_stale_body_replacement_is_refused_rather_than_merged() {
    let root = tempfile::tempdir().expect("temporary test root");
    let store = V2Store::init(root.path().join("library"))
        .await
        .expect("store");
    let created = store
        .create_zen_document(CreateZenDocumentRequest {
            body: "first line\n".into(),
            ..CreateZenDocumentRequest::default()
        })
        .await
        .expect("create document");

    // What an editor opened, before anything else wrote.
    let opened = "first line\n".to_owned();
    store
        .edit_zen_document(EditZenDocumentRequest {
            document_id: created.document_id.clone(),
            body: Some("first line\nfrom the other device\n".into()),
            ..EditZenDocumentRequest::default()
        })
        .await
        .expect("concurrent edit");

    let stale = store
        .edit_zen_document(EditZenDocumentRequest {
            document_id: created.document_id.clone(),
            body: Some("first line\nfrom the stale buffer\n".into()),
            expected_body: Some(opened),
            ..EditZenDocumentRequest::default()
        })
        .await;
    assert!(
        matches!(stale, Err(StoreError::StaleEdit)),
        "expected a refused stale edit, got {stale:?}"
    );
    assert_eq!(
        store
            .zen_document(&created.document_id)
            .await
            .expect("read back")
            .body,
        "first line\nfrom the other device\n"
    );

    // The same write succeeds once it is sent against what the replica holds.
    store
        .edit_zen_document(EditZenDocumentRequest {
            document_id: created.document_id.clone(),
            body: Some("agreed\n".into()),
            expected_body: Some("first line\nfrom the other device\n".into()),
            ..EditZenDocumentRequest::default()
        })
        .await
        .expect("fresh edit");
}
