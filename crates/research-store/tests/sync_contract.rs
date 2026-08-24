use research_domain::{UpdateEnvelope, create_operation_pack};
use research_store::{
    CreateItemRequest, EditItemRequest, ListQuery, OptionalTextUpdate, RemoteBatchDisposition,
    SearchQuery, StoreError, V2Store,
};

#[tokio::test]
async fn checkpoint_bootstrap_restores_coverage_and_applies_only_the_tail() {
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

    let item = sender
        .create_item(CreateItemRequest {
            url: "https://example.com/checkpoint".into(),
            title: Some("Checkpoint".into()),
            excerpt: Some("bounded state".into()),
            favorite: false,
            language: Some("en".into()),
            saved_at: Some(1_700_000_000),
            note: String::new(),
            tags: vec!["restore".into()],
        })
        .await
        .expect("create checkpoint item");
    sender
        .edit_item(EditItemRequest {
            item_id: item.id.clone(),
            favorite: Some(true),
            ..EditItemRequest::default()
        })
        .await
        .expect("edit before checkpoint");
    let covered = sender.pending_batches().await.expect("covered batches");
    let checkpoint = sender
        .checkpoint_candidate(true)
        .await
        .expect("build checkpoint")
        .expect("checkpoint candidate");

    let restored = receiver
        .receive_remote_checkpoint(
            &checkpoint.path,
            &"a".repeat(40),
            checkpoint.checkpoint_json.as_bytes(),
        )
        .await
        .expect("restore checkpoint");
    assert!(restored.restored);
    assert_eq!(restored.batch_count, 2);
    assert_eq!(
        receiver
            .list(ListQuery::default())
            .await
            .expect("restored projection"),
        sender
            .list(ListQuery::default())
            .await
            .expect("sender projection")
    );

    for batch in &covered {
        assert!(
            receiver
                .batch_is_checkpoint_covered(&batch.device_id, &batch.sequence)
                .await
                .expect("coverage lookup")
        );
        let result = receiver
            .receive_remote_batch(&batch.path, &"b".repeat(40), batch.envelope_json.as_bytes())
            .await
            .expect("covered operation is idempotent");
        assert_eq!(result.disposition, RemoteBatchDisposition::AlreadyApplied);
    }

    sender
        .edit_item(EditItemRequest {
            item_id: item.id,
            title: Some(OptionalTextUpdate::Set("After checkpoint".into())),
            ..EditItemRequest::default()
        })
        .await
        .expect("create uncovered tail");
    let tail = sender
        .pending_batches()
        .await
        .expect("tail outbox")
        .into_iter()
        .find(|batch| !covered.iter().any(|covered| covered.path == batch.path))
        .expect("uncovered tail batch");
    let applied = receiver
        .receive_remote_batch(&tail.path, &"c".repeat(40), tail.envelope_json.as_bytes())
        .await
        .expect("apply uncovered tail");
    assert_eq!(applied.disposition, RemoteBatchDisposition::Applied);
    assert_eq!(
        receiver
            .item(
                &sender
                    .list(ListQuery::default())
                    .await
                    .expect("sender list")
                    .items[0]
                    .id
            )
            .await
            .expect("receiver item")
            .title
            .as_deref(),
        Some("After checkpoint")
    );
}

/// Migration seeds the v2 generation without disturbing retained v1 history.
///
/// Repeating it must produce the same identities rather than a second
/// generation, because the first attempt may have committed locally and then
/// failed before anything was uploaded.
#[tokio::test]
async fn migrating_to_item_aggregates_is_idempotent_and_queues_the_barrier() {
    let root = tempfile::tempdir().expect("temporary test root");
    let store = V2Store::init(root.path().join("library"))
        .await
        .expect("store");
    for url in ["https://example.com/one", "https://example.com/two"] {
        store
            .create_item(CreateItemRequest {
                url: url.into(),
                title: Some("Saved".into()),
                excerpt: None,
                favorite: false,
                language: None,
                saved_at: None,
                note: String::new(),
                tags: vec!["reference".into()],
            })
            .await
            .expect("create item");
    }
    // A device with unsent work cannot migrate: those operations would have to
    // be re-expressed in a generation that did not exist when they were made.
    assert!(store.migrate_to_item_aggregates().await.is_err());
    // Pulling back an uploaded operation is what acknowledges the outbox.
    for batch in store.pending_batches().await.expect("pending") {
        store
            .receive_remote_batch(&batch.path, &"a".repeat(40), batch.envelope_json.as_bytes())
            .await
            .expect("confirm upload");
    }

    let receipt = store
        .migrate_to_item_aggregates()
        .await
        .expect("migrate to aggregates");
    assert!(!receipt.already_migrated);
    assert_eq!(receipt.aggregate_count, 2, "one aggregate per item");

    // The barrier is an ordinary v1 operation, so it uploads through the
    // existing outbox and older clients meet it on their next pull.
    let queued = store.pending_batches().await.expect("barrier queued");
    assert_eq!(queued.len(), 1);
    assert_eq!(Some(queued[0].path.clone()), receipt.barrier_path);

    let repeated = store
        .migrate_to_item_aggregates()
        .await
        .expect("repeat migration");
    assert!(repeated.already_migrated);
    assert_eq!(repeated.v1_checkpoint_id, receipt.v1_checkpoint_id);
    assert_eq!(repeated.catalogue_sha256, receipt.catalogue_sha256);
    assert_eq!(
        store
            .list(ListQuery::default())
            .await
            .expect("projection")
            .page
            .total,
        2,
        "migration leaves the readable library untouched"
    );
}

/// A device that edits while its own checkpoint uploads must still select it.
///
/// The tail is measured from the selected checkpoint, so failing to select here
/// would mint and upload another full snapshot on every later sync, growing the
/// data repository without bound.
#[tokio::test]
async fn a_self_created_checkpoint_is_selected_after_a_concurrent_local_edit() {
    let root = tempfile::tempdir().expect("temporary test root");
    let store = V2Store::init(root.path().join("library"))
        .await
        .expect("store");
    let item = store
        .create_item(CreateItemRequest {
            url: "https://example.com/racing-edit".into(),
            title: Some("Before checkpoint".into()),
            excerpt: None,
            favorite: false,
            language: None,
            saved_at: None,
            note: String::new(),
            tags: Vec::new(),
        })
        .await
        .expect("create item");
    let covered = store.pending_batches().await.expect("covered batches");
    let checkpoint = store
        .checkpoint_candidate(true)
        .await
        .expect("build checkpoint")
        .expect("checkpoint candidate");

    // The edit lands between building the checkpoint and confirming its upload,
    // so canonical state no longer equals the checkpoint snapshot.
    store
        .edit_item(EditItemRequest {
            item_id: item.id,
            title: Some(OptionalTextUpdate::Set("During upload".into())),
            ..EditItemRequest::default()
        })
        .await
        .expect("edit during upload");

    let confirmed = store
        .receive_remote_checkpoint(
            &checkpoint.path,
            &"d".repeat(40),
            checkpoint.checkpoint_json.as_bytes(),
        )
        .await
        .expect("confirm own checkpoint");
    assert!(
        !confirmed.restored,
        "a non-pristine store must not replace its own newer state"
    );
    for batch in &covered {
        assert!(
            store
                .batch_is_checkpoint_covered(&batch.device_id, &batch.sequence)
                .await
                .expect("coverage lookup"),
            "the checkpoint must be selected so its coverage counts as covered"
        );
    }
}

#[tokio::test]
async fn native_mutations_reuse_one_durable_loro_peer() {
    let root = tempfile::tempdir().expect("temporary test root");
    let store = V2Store::init(root.path().join("library"))
        .await
        .expect("initialize store");
    let item = store
        .create_item(CreateItemRequest {
            url: "https://example.com/stable-peer".into(),
            title: None,
            excerpt: None,
            favorite: false,
            language: None,
            saved_at: Some(1_700_000_000),
            note: String::new(),
            tags: Vec::new(),
        })
        .await
        .expect("create item");
    for favorite in [true, false] {
        store
            .edit_item(EditItemRequest {
                item_id: item.id.clone(),
                favorite: Some(favorite),
                ..EditItemRequest::default()
            })
            .await
            .expect("edit item");
    }
    let envelopes = store
        .pending_batches()
        .await
        .expect("pending batches")
        .into_iter()
        .map(|batch| {
            serde_json::from_str::<UpdateEnvelope>(&batch.envelope_json)
                .expect("stored update envelope")
        })
        .collect::<Vec<_>>();
    assert_eq!(envelopes.len(), 3);
    assert!(envelopes[0].causal_frontier.is_empty());
    assert_eq!(envelopes[1].causal_frontier.len(), 1);
    assert_eq!(envelopes[2].causal_frontier.len(), 1);
    assert_eq!(
        envelopes[1].causal_frontier.keys().collect::<Vec<_>>(),
        envelopes[2].causal_frontier.keys().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn one_operation_pack_applies_and_acknowledges_several_exact_updates() {
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
        .expect("receiver adopts library");

    let item = sender
        .create_item(CreateItemRequest {
            url: "https://example.com/packed".into(),
            title: Some("Before packing".into()),
            excerpt: None,
            favorite: false,
            language: None,
            saved_at: Some(1_700_000_000),
            note: String::new(),
            tags: vec![],
        })
        .await
        .expect("create item");
    sender
        .edit_item(EditItemRequest {
            item_id: item.id.clone(),
            favorite: Some(true),
            ..EditItemRequest::default()
        })
        .await
        .expect("favorite item");
    sender
        .edit_item(EditItemRequest {
            item_id: item.id,
            title: Some(OptionalTextUpdate::Set("After packing".into())),
            ..EditItemRequest::default()
        })
        .await
        .expect("retitle item");

    let pending = sender.pending_batches().await.expect("sender outbox");
    assert_eq!(pending.len(), 3);
    let artifact = create_operation_pack(
        &pending
            .iter()
            .map(|batch| batch.envelope_json.clone())
            .collect::<Vec<_>>(),
    )
    .expect("build operation pack");
    let pack_blob_sha = "a".repeat(40);

    let applied = receiver
        .receive_remote_pack(&artifact.path, &pack_blob_sha, artifact.json.as_bytes())
        .await
        .expect("apply pack");
    assert_eq!(applied.member_count, 3);
    assert_eq!(applied.applied, 3);
    assert_eq!(applied.already_applied, 0);
    assert_eq!(applied.acknowledged_outbox, 0);
    let received = receiver
        .list(ListQuery::default())
        .await
        .expect("receiver projection");
    assert_eq!(received.items.len(), 1);
    assert!(received.items[0].favorite);
    assert_eq!(received.items[0].title.as_deref(), Some("After packing"));

    let acknowledged = sender
        .receive_remote_pack(&artifact.path, &pack_blob_sha, artifact.json.as_bytes())
        .await
        .expect("acknowledge packed upload");
    assert_eq!(acknowledged.already_applied, 3);
    assert_eq!(acknowledged.acknowledged_outbox, 3);
    assert!(
        sender
            .pending_batches()
            .await
            .expect("drained sender outbox")
            .is_empty()
    );

    let before_tamper = receiver
        .list(ListQuery::default())
        .await
        .expect("projection before tamper");
    let mut tampered = artifact.json.into_bytes();
    let last = tampered.last_mut().expect("non-empty pack");
    *last = if *last == b'}' { b']' } else { b'}' };
    assert!(
        receiver
            .receive_remote_pack(&artifact.path, &"b".repeat(40), &tampered)
            .await
            .is_err()
    );
    assert_eq!(
        receiver
            .list(ListQuery::default())
            .await
            .expect("projection after rejected tamper"),
        before_tamper
    );
}

#[tokio::test]
async fn remote_replay_is_exact_idempotent_and_convergent() {
    let root = tempfile::tempdir().expect("temporary test root");
    let first = V2Store::init(root.path().join("first"))
        .await
        .expect("first store");
    let second = V2Store::init(root.path().join("second"))
        .await
        .expect("second store");
    let first_identity = first.sync_identity().await.expect("first identity");
    let second_device = second
        .sync_identity()
        .await
        .expect("second identity")
        .device_id;
    assert!(
        second
            .adopt_library_id_if_pristine(&first_identity.library_id)
            .await
            .expect("adopt remote library")
    );
    let adopted = second.sync_identity().await.expect("adopted identity");
    assert_eq!(adopted.library_id, first_identity.library_id);
    assert_eq!(adopted.device_id, second_device);
    let configuration = second
        .configure_sync("owner", "private-library", "main")
        .await
        .expect("configure synchronization");
    assert_eq!(
        second
            .configure_sync("owner", "private-library", "main")
            .await
            .expect("repeat same configuration"),
        configuration
    );
    assert!(matches!(
        second
            .configure_sync("owner", "another-library", "main")
            .await
            .expect_err("remote replacement must be explicit"),
        StoreError::InvalidInput(_)
    ));
    second
        .record_immutable_remote_blob("sync/v1/library.json", &"f".repeat(40))
        .await
        .expect("record immutable genesis");
    assert!(matches!(
        second
            .record_immutable_remote_blob("sync/v1/library.json", &"e".repeat(40))
            .await
            .expect_err("genesis identity must not change"),
        StoreError::SyncIntegrity(_)
    ));

    let item = first
        .create_item(CreateItemRequest {
            url: "https://example.com/sync-contract".into(),
            title: Some("Initial title".into()),
            excerpt: None,
            favorite: false,
            language: None,
            saved_at: Some(1_700_000_000),
            note: "private note".into(),
            tags: vec!["sync".into()],
        })
        .await
        .expect("create initial item");
    let initial = first
        .pending_batches()
        .await
        .expect("initial outbox")
        .remove(0);
    let initial_sha = "a".repeat(40);
    let applied = second
        .receive_remote_batch(
            &initial.path,
            &initial_sha,
            initial.envelope_json.as_bytes(),
        )
        .await
        .expect("apply initial remote batch");
    assert_eq!(applied.disposition, RemoteBatchDisposition::Applied);
    let duplicate = second
        .receive_remote_batch(
            &initial.path,
            &initial_sha,
            initial.envelope_json.as_bytes(),
        )
        .await
        .expect("repeat initial remote batch");
    assert_eq!(
        duplicate.disposition,
        RemoteBatchDisposition::AlreadyApplied
    );
    let acknowledged = first
        .receive_remote_batch(
            &initial.path,
            &initial_sha,
            initial.envelope_json.as_bytes(),
        )
        .await
        .expect("confirm initial upload");
    assert!(acknowledged.acknowledged_outbox);

    first
        .edit_item(EditItemRequest {
            item_id: item.id.clone(),
            favorite: Some(true),
            ..EditItemRequest::default()
        })
        .await
        .expect("first concurrent edit");
    second
        .edit_item(EditItemRequest {
            item_id: item.id.clone(),
            title: Some(OptionalTextUpdate::Set("Remote title".into())),
            ..EditItemRequest::default()
        })
        .await
        .expect("second concurrent edit");
    let first_edit = first
        .pending_batches()
        .await
        .expect("first edit outbox")
        .remove(0);
    let second_edit = second
        .pending_batches()
        .await
        .expect("second edit outbox")
        .remove(0);
    let first_edit_sha = "b".repeat(40);
    let second_edit_sha = "c".repeat(40);

    let reordered = V2Store::init(root.path().join("reordered"))
        .await
        .expect("reordered store");
    reordered
        .adopt_library_id_if_pristine(&first_identity.library_id)
        .await
        .expect("adopt reordered library");
    reordered
        .receive_remote_batch(
            &second_edit.path,
            &second_edit_sha,
            second_edit.envelope_json.as_bytes(),
        )
        .await
        .expect("accept causally later batch first");
    reordered
        .receive_remote_batch(
            &initial.path,
            &initial_sha,
            initial.envelope_json.as_bytes(),
        )
        .await
        .expect("accept causal predecessor later");
    let reordered_projection = reordered
        .list(ListQuery {
            include_deleted: true,
            ..ListQuery::default()
        })
        .await
        .expect("projection after reordered replay");
    assert_eq!(reordered_projection.items.len(), 1);
    assert_eq!(
        reordered_projection.items[0].title.as_deref(),
        Some("Remote title")
    );
    assert_eq!(
        reordered
            .status()
            .await
            .expect("reordered sync status")
            .deferred_updates,
        0
    );

    first
        .receive_remote_batch(
            &second_edit.path,
            &second_edit_sha,
            second_edit.envelope_json.as_bytes(),
        )
        .await
        .expect("first receives second edit");
    second
        .receive_remote_batch(
            &first_edit.path,
            &first_edit_sha,
            first_edit.envelope_json.as_bytes(),
        )
        .await
        .expect("second receives first edit");
    first
        .receive_remote_batch(
            &first_edit.path,
            &first_edit_sha,
            first_edit.envelope_json.as_bytes(),
        )
        .await
        .expect("first edit upload confirmation");
    second
        .receive_remote_batch(
            &second_edit.path,
            &second_edit_sha,
            second_edit.envelope_json.as_bytes(),
        )
        .await
        .expect("second edit upload confirmation");

    let query = ListQuery {
        include_deleted: true,
        ..ListQuery::default()
    };
    let first_projection = first.list(query.clone()).await.expect("first projection");
    let second_projection = second.list(query).await.expect("second projection");
    assert_eq!(first_projection, second_projection);
    assert!(first_projection.items[0].favorite);
    assert_eq!(
        first_projection.items[0].title.as_deref(),
        Some("Remote title")
    );
    assert_eq!(
        second
            .search(SearchQuery {
                text: "Remote title".into(),
                ..SearchQuery::default()
            })
            .await
            .expect("remote projection search")
            .page
            .total,
        1
    );
    assert!(
        first
            .pending_batches()
            .await
            .expect("first drained")
            .is_empty()
    );
    assert!(
        second
            .pending_batches()
            .await
            .expect("second drained")
            .is_empty()
    );

    let mut collision: serde_json::Value =
        serde_json::from_str(&second_edit.envelope_json).expect("stored envelope JSON");
    collision["created_at"] = serde_json::Value::String("2026-07-11T01:02:03.000Z".into());
    let collision = serde_json::to_vec(&collision).expect("collision JSON");
    let error = second
        .receive_remote_batch(&second_edit.path, &"d".repeat(40), &collision)
        .await
        .expect_err("byte-different identity collision must fail");
    assert!(matches!(error, StoreError::SyncIntegrity(_)));
    assert_eq!(
        second
            .list(ListQuery {
                include_deleted: true,
                ..ListQuery::default()
            })
            .await
            .expect("projection after rejected collision"),
        second_projection
    );
}

#[tokio::test]
async fn reverse_ordered_dependency_chain_retries_to_a_fixed_point() {
    let root = tempfile::tempdir().expect("temporary test root");
    let base = V2Store::init(root.path().join("base"))
        .await
        .expect("base store");
    let first_peer = V2Store::init(root.path().join("first-peer"))
        .await
        .expect("first peer");
    let second_peer = V2Store::init(root.path().join("second-peer"))
        .await
        .expect("second peer");
    let first_id = first_peer
        .sync_identity()
        .await
        .expect("first identity")
        .device_id;
    let second_id = second_peer
        .sync_identity()
        .await
        .expect("second identity")
        .device_id;
    let (leaf, middle) = if first_id < second_id {
        (&first_peer, &second_peer)
    } else {
        (&second_peer, &first_peer)
    };
    let identity = base.sync_identity().await.expect("base identity");
    for peer in [leaf, middle] {
        peer.adopt_library_id_if_pristine(&identity.library_id)
            .await
            .expect("adopt base library");
    }

    let item = base
        .create_item(CreateItemRequest {
            url: "https://example.com/dependency-chain".into(),
            title: Some("Base".into()),
            excerpt: None,
            favorite: false,
            language: None,
            saved_at: None,
            note: String::new(),
            tags: Vec::new(),
        })
        .await
        .expect("create base item");
    let predecessor = base.pending_batches().await.expect("base outbox").remove(0);
    middle
        .receive_remote_batch(
            &predecessor.path,
            &"a".repeat(40),
            predecessor.envelope_json.as_bytes(),
        )
        .await
        .expect("middle receives predecessor");
    middle
        .edit_item(EditItemRequest {
            item_id: item.id.clone(),
            title: Some(OptionalTextUpdate::Set("Middle".into())),
            ..EditItemRequest::default()
        })
        .await
        .expect("create middle operation");
    let middle_batch = middle
        .pending_batches()
        .await
        .expect("middle outbox")
        .remove(0);

    leaf.receive_remote_batch(
        &predecessor.path,
        &"a".repeat(40),
        predecessor.envelope_json.as_bytes(),
    )
    .await
    .expect("leaf receives predecessor");
    leaf.receive_remote_batch(
        &middle_batch.path,
        &"b".repeat(40),
        middle_batch.envelope_json.as_bytes(),
    )
    .await
    .expect("leaf receives middle operation");
    leaf.edit_item(EditItemRequest {
        item_id: item.id.clone(),
        favorite: Some(true),
        ..EditItemRequest::default()
    })
    .await
    .expect("create leaf operation");
    let leaf_batch = leaf.pending_batches().await.expect("leaf outbox").remove(0);

    let receiver = V2Store::init(root.path().join("receiver"))
        .await
        .expect("receiver store");
    receiver
        .adopt_library_id_if_pristine(&identity.library_id)
        .await
        .expect("adopt receiver library");
    receiver
        .receive_remote_batch(
            &leaf_batch.path,
            &"c".repeat(40),
            leaf_batch.envelope_json.as_bytes(),
        )
        .await
        .expect("receive leaf first");
    receiver
        .receive_remote_batch(
            &middle_batch.path,
            &"b".repeat(40),
            middle_batch.envelope_json.as_bytes(),
        )
        .await
        .expect("receive middle second");
    assert_eq!(receiver.retry_deferred_batches().await.expect("retry"), 2);

    receiver
        .receive_remote_batch(
            &predecessor.path,
            &"a".repeat(40),
            predecessor.envelope_json.as_bytes(),
        )
        .await
        .expect("receive predecessor last");
    assert_eq!(receiver.retry_deferred_batches().await.expect("retry"), 0);
    assert_eq!(
        receiver
            .status()
            .await
            .expect("receiver status")
            .deferred_updates,
        0
    );
    let restored = receiver.item(&item.id).await.expect("restored item");
    assert_eq!(restored.title.as_deref(), Some("Middle"));
    assert!(restored.favorite);
}
