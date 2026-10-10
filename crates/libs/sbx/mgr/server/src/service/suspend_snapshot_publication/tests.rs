use super::*;

const TEST_SANDBOX_ID: &str = "019e31d9-79a6-7132-b90f-007f1c62ddea";

#[test]
fn publication_replaces_the_complete_snapshot_and_preserves_runtime_siblings() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let sandbox_dir = tmp.path();
    let snapshot_dir = canonical_suspend_snapshot_dir(sandbox_dir);
    let staging_dir = sandbox_dir.join("publish-source");
    let instance_dir = sandbox_dir.join("instance");
    std::fs::create_dir_all(&instance_dir).unwrap();

    write_full_suspend_artifact(
        &snapshot_dir,
        TEST_SANDBOX_ID,
        LocalArtifactId::new(),
        b"old-artifact",
    );
    let new_artifact_id = LocalArtifactId::new();
    write_full_suspend_artifact(
        &staging_dir,
        TEST_SANDBOX_ID,
        new_artifact_id,
        b"new-artifact",
    );
    std::fs::write(instance_dir.join("rootfs.ext4"), b"live-fc-rootfs").unwrap();

    publish_suspend_snapshot_dir(
        &staging_dir,
        &snapshot_dir,
        new_artifact_id,
        &Default::default(),
    )
    .unwrap();

    assert_eq!(
        std::fs::read(snapshot_dir.join("vmstate")).unwrap(),
        b"new-artifact"
    );
    assert!(!staging_dir.exists());

    let live = instance_dir.join("rootfs.ext4");
    assert!(live.exists(), "publication must not touch runtime siblings");
    assert_eq!(
        std::fs::read(&live).unwrap(),
        b"live-fc-rootfs",
        "publication must not rewrite the live rootfs",
    );
}

#[test]
fn publication_makes_a_new_snapshot_visible_only_at_publish_time() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let sandbox_dir = tmp.path();
    let snapshot_dir = canonical_suspend_snapshot_dir(sandbox_dir);
    let staging_dir = sandbox_dir.join("publish-source");
    let local_artifact_id = LocalArtifactId::new();
    write_full_suspend_artifact(
        &staging_dir,
        TEST_SANDBOX_ID,
        local_artifact_id,
        b"complete-artifact",
    );

    assert!(!snapshot_dir.exists());
    publish_suspend_snapshot_dir(
        &staging_dir,
        &snapshot_dir,
        local_artifact_id,
        &Default::default(),
    )
    .unwrap();

    assert_eq!(
        std::fs::read(snapshot_dir.join("vmstate")).unwrap(),
        b"complete-artifact"
    );
}

#[test]
fn publication_is_idempotent_when_canonical_already_matches_artifact() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let sandbox_dir = tmp.path();
    let snapshot_dir = canonical_suspend_snapshot_dir(sandbox_dir);
    let staging_dir = sandbox_dir.join("publish-source");
    let local_artifact_id = LocalArtifactId::new();
    write_full_suspend_artifact(
        &snapshot_dir,
        TEST_SANDBOX_ID,
        local_artifact_id,
        b"canonical-artifact",
    );

    publish_suspend_snapshot_dir(
        &staging_dir,
        &snapshot_dir,
        local_artifact_id,
        &Default::default(),
    )
    .expect("retry after a completed publish must be idempotent");

    assert_eq!(
        std::fs::read(snapshot_dir.join("vmstate")).unwrap(),
        b"canonical-artifact"
    );
    assert!(!staging_dir.exists());
}

#[test]
fn classifier_prefers_complete_staging_over_unpinned_canonical_artifact() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let sandbox_dir = tmp.path();
    let old_artifact_id = LocalArtifactId::new();
    let new_artifact_id = LocalArtifactId::new();
    write_full_suspend_artifact(
        &canonical_suspend_snapshot_dir(sandbox_dir),
        TEST_SANDBOX_ID,
        old_artifact_id,
        b"old-canonical",
    );
    let staging_dir = suspend_snapshot_staging_dir(sandbox_dir, new_artifact_id);
    write_full_suspend_artifact(
        &staging_dir,
        TEST_SANDBOX_ID,
        new_artifact_id,
        b"new-staging",
    );

    let decision =
        classify_suspend_snapshot_publication(sandbox_dir, None, false, &Default::default())
            .unwrap();

    assert_eq!(
        decision,
        SuspendSnapshotPublicationDecision::StagedComplete(StagedSuspendSnapshotArtifact {
            local_artifact_id: new_artifact_id,
            staging_dir
        })
    );
}

#[test]
fn classifier_refuses_mismatched_canonical_when_expected_artifact_is_known() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let sandbox_dir = tmp.path();
    let expected_artifact_id = LocalArtifactId::new();
    let stale_artifact_id = LocalArtifactId::new();
    write_full_suspend_artifact(
        &canonical_suspend_snapshot_dir(sandbox_dir),
        TEST_SANDBOX_ID,
        stale_artifact_id,
        b"stale-canonical",
    );

    let decision = classify_suspend_snapshot_publication(
        sandbox_dir,
        Some(expected_artifact_id),
        false,
        &Default::default(),
    )
    .unwrap();

    assert_eq!(
        decision,
        SuspendSnapshotPublicationDecision::RuntimeGoneWithoutUsableArtifact,
        "mismatched canonical artifact must not be adopted when DB names an expected artifact"
    );
}

#[test]
fn classifier_treats_runtime_live_as_source_of_truth() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let sandbox_dir = tmp.path();
    let local_artifact_id = LocalArtifactId::new();
    let staging_dir = suspend_snapshot_staging_dir(sandbox_dir, local_artifact_id);
    write_full_suspend_artifact(
        &staging_dir,
        TEST_SANDBOX_ID,
        local_artifact_id,
        b"complete-staging",
    );

    let decision =
        classify_suspend_snapshot_publication(sandbox_dir, None, true, &Default::default())
            .unwrap();

    assert_eq!(
        decision,
        SuspendSnapshotPublicationDecision::RuntimeLiveWithoutUsableArtifact
    );
}

fn retained(snapshot_dir: &Path, source_id: uuid::Uuid) -> RetainedSource {
    RetainedSource {
        sandbox_id: TEST_SANDBOX_ID.to_owned(),
        artifact_dir: snapshot_dir.to_owned(),
        source_id,
    }
}

#[test]
fn deferred_artifact_completeness_follows_node_manager_verdicts() {
    let tmp = tempfile::tempdir().unwrap();
    let snapshot_dir = tmp.path().join("snapshot");
    let source_id = uuid::Uuid::now_v7();
    let _bundle = write_deferred_suspend_artifact(
        &snapshot_dir,
        TEST_SANDBOX_ID,
        LocalArtifactId::new(),
        source_id,
        100,
    );
    let validate = |check: Option<RetainedSourceCheck>| {
        let mut verdicts = RetainedSourceVerdicts::default();
        if let Some(check) = check {
            verdicts.record(retained(&snapshot_dir, source_id), check);
        }
        let result = validate_complete_suspend_snapshot_dir(&snapshot_dir, &verdicts).map(|_| ());
        (result, verdicts.take_unresolved())
    };

    let (result, unresolved) = validate(Some(RetainedSourceCheck::Present { rootfs_bytes: 100 }));
    result.unwrap();
    assert!(unresolved.is_empty());

    let (result, _) = validate(Some(RetainedSourceCheck::Present { rootfs_bytes: 99 }));
    let error = result.unwrap_err();
    assert!(matches!(
        error,
        SuspendSnapshotValidationError::ArtifactSizeMismatch { .. }
    ));
    assert!(error.is_unsafe_evidence());

    let (result, _) = validate(Some(RetainedSourceCheck::Absent));
    let error = result.unwrap_err();
    assert!(matches!(
        error,
        SuspendSnapshotValidationError::MissingArtifact {
            artifact: crate::retained_source::BUNDLE_DIRECTORY,
            ..
        }
    ));
    assert!(error.is_unsafe_evidence());

    let (result, _) = validate(Some(RetainedSourceCheck::Invalid {
        reason: "identity changed".to_owned(),
    }));
    let error = result.unwrap_err();
    assert!(matches!(
        error,
        SuspendSnapshotValidationError::RetainedSourceInvalid { .. }
    ));
    assert!(error.is_unsafe_evidence());

    for check in [
        Some(RetainedSourceCheck::Unavailable {
            reason: "node-manager restarting".to_owned(),
        }),
        None,
    ] {
        let (result, unresolved) = validate(check);
        let error = result.unwrap_err();
        assert!(error.is_pending(), "{error}");
        assert!(!error.is_unsafe_evidence(), "{error}");
        assert_eq!(unresolved, vec![retained(&snapshot_dir, source_id)]);
    }
}

#[test]
fn classification_waits_for_an_unverified_retained_source() {
    let tmp = tempfile::tempdir().unwrap();
    let sandbox_dir = tmp.path();
    let snapshot_dir = canonical_suspend_snapshot_dir(sandbox_dir);
    let local_artifact_id = LocalArtifactId::new();
    let source_id = uuid::Uuid::now_v7();
    let _bundle = write_deferred_suspend_artifact(
        &snapshot_dir,
        TEST_SANDBOX_ID,
        local_artifact_id,
        source_id,
        8,
    );
    let classify = |check: Option<RetainedSourceCheck>| {
        let mut verdicts = RetainedSourceVerdicts::default();
        if let Some(check) = check {
            verdicts.record(retained(&snapshot_dir, source_id), check);
        }
        classify_suspend_snapshot_publication(
            sandbox_dir,
            Some(local_artifact_id),
            false,
            &verdicts,
        )
    };

    // Without an answer the runtime-gone conclusion would quarantine an
    // artifact that may be complete.
    assert!(matches!(
        classify(None),
        Err(SuspendSnapshotPublicationError::Validation(error)) if error.is_pending()
    ));
    assert!(matches!(
        classify(Some(RetainedSourceCheck::Present { rootfs_bytes: 8 })).unwrap(),
        SuspendSnapshotPublicationDecision::CanonicalReady(artifact)
            if artifact.local_artifact_id == local_artifact_id
    ));
    assert_eq!(
        classify(Some(RetainedSourceCheck::Absent)).unwrap(),
        SuspendSnapshotPublicationDecision::RuntimeGoneWithoutUsableArtifact
    );
}

#[test]
fn retained_sources_are_discovered_from_canonical_and_staged_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let sandbox_dir = tmp.path();
    let canonical = canonical_suspend_snapshot_dir(sandbox_dir);
    let staged = suspend_snapshot_staging_dir(sandbox_dir, LocalArtifactId::new());
    let full = suspend_snapshot_staging_dir(sandbox_dir, LocalArtifactId::new());
    let (canonical_id, staged_id) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    let _canonical = write_deferred_suspend_artifact(
        &canonical,
        TEST_SANDBOX_ID,
        LocalArtifactId::new(),
        canonical_id,
        1,
    );
    let _staged = write_deferred_suspend_artifact(
        &staged,
        TEST_SANDBOX_ID,
        LocalArtifactId::new(),
        staged_id,
        1,
    );
    write_full_suspend_artifact(&full, TEST_SANDBOX_ID, LocalArtifactId::new(), b"full");

    let mut found = retained_sources_in_sandbox_dir(sandbox_dir);
    found.sort();
    let mut expected = vec![
        retained(&canonical, canonical_id),
        retained(&staged, staged_id),
    ];
    expected.sort();
    assert_eq!(found, expected);
}
