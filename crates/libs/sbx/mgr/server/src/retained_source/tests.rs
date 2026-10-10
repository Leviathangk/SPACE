use super::*;

const SANDBOX_ID: &str = "01900000-0000-7000-8000-000000000001";

/// A bundle this process cannot traverse, standing in for the sandbox jail's
/// ownership. Root bypasses the mode, so the absent stream file and the
/// resolution's result carry the assertion there.
fn inaccessible_bundle(artifact_dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let source = artifact_dir.join(BUNDLE_DIRECTORY).join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("rootfs.ext4"), b"jail-owned rootfs").unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::set_permissions(
        artifact_dir.join(BUNDLE_DIRECTORY),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
}

fn restore_access(artifact_dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let bundle = artifact_dir.join(BUNDLE_DIRECTORY);
    std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o750)).unwrap();
    std::fs::set_permissions(
        bundle.join("source"),
        std::fs::Permissions::from_mode(0o750),
    )
    .unwrap();
}

fn source(artifact_dir: &Path, source_id: uuid::Uuid) -> RetainedSource {
    RetainedSource {
        sandbox_id: SANDBOX_ID.to_owned(),
        artifact_dir: artifact_dir.to_owned(),
        source_id,
    }
}

#[test]
fn resolution_names_the_retained_source_from_identity_without_entering_its_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let artifact_dir = temp.path();
    inaccessible_bundle(artifact_dir);
    let source_id = uuid::Uuid::now_v7();

    assert_eq!(
        RootfsArtifact::resolve(SANDBOX_ID, artifact_dir, Some(source_id)).unwrap(),
        Some(RootfsArtifact::RetainedSource(source(
            artifact_dir,
            source_id
        )))
    );
    // A bundle without a persisted identity is not a usable rootfs.
    assert_eq!(
        RootfsArtifact::resolve(SANDBOX_ID, artifact_dir, None).unwrap(),
        None
    );
    // A stream file wins over the retained source it regenerates from.
    let stream = artifact_dir.join(ROOTFS_BTRFS_SEND_FILENAME);
    std::fs::write(&stream, b"stream").unwrap();
    assert_eq!(
        RootfsArtifact::resolve(SANDBOX_ID, artifact_dir, Some(source_id)).unwrap(),
        Some(RootfsArtifact::SendFile { path: stream })
    );
    restore_access(artifact_dir);
}

#[test]
fn node_manager_status_maps_to_absent_invalid_or_unavailable() {
    assert_eq!(
        RetainedSourceCheck::from_status(&tonic::Status::not_found("gone")),
        RetainedSourceCheck::Absent
    );
    assert_eq!(
        RetainedSourceCheck::from_status(&tonic::Status::failed_precondition("changed")),
        RetainedSourceCheck::Invalid {
            reason: "changed".to_owned()
        }
    );
    // An older node-manager, an overloaded one, or a transient fault proves
    // nothing about the artifact.
    for status in [
        tonic::Status::unimplemented("old"),
        tonic::Status::resource_exhausted("busy"),
        tonic::Status::internal("EIO"),
        tonic::Status::unavailable("restarting"),
        tonic::Status::permission_denied("wrong peer"),
    ] {
        assert!(matches!(
            RetainedSourceCheck::from_status(&status),
            RetainedSourceCheck::Unavailable { .. }
        ));
    }
}

#[test]
fn verdicts_record_every_source_validation_could_not_resolve() {
    let temp = tempfile::tempdir().unwrap();
    let present = source(temp.path(), uuid::Uuid::now_v7());
    let unavailable = source(temp.path(), uuid::Uuid::now_v7());
    let missing = source(temp.path(), uuid::Uuid::now_v7());
    let mut verdicts = RetainedSourceVerdicts::default();
    verdicts.record(
        present.clone(),
        RetainedSourceCheck::Present { rootfs_bytes: 7 },
    );
    verdicts.record(
        unavailable.clone(),
        RetainedSourceCheck::Unavailable {
            reason: "timeout".to_owned(),
        },
    );

    assert_eq!(
        verdicts.check(&present),
        RetainedSourceCheck::Present { rootfs_bytes: 7 }
    );
    assert!(matches!(
        verdicts.check(&unavailable),
        RetainedSourceCheck::Unavailable { .. }
    ));
    assert!(matches!(
        verdicts.check(&missing),
        RetainedSourceCheck::Unavailable { .. }
    ));
    let mut expected = vec![unavailable, missing];
    expected.sort();
    assert_eq!(verdicts.take_unresolved(), expected);
    assert!(verdicts.take_unresolved().is_empty());
}

struct SlowInspector;

#[async_trait::async_trait]
impl RetainedSourceInspector for SlowInspector {
    async fn inspect(&self, _source: &RetainedSource) -> RetainedSourceCheck {
        tokio::time::sleep(Duration::from_secs(1)).await;
        RetainedSourceCheck::Present { rootfs_bytes: 1 }
    }
}

#[tokio::test(start_paused = true)]
async fn background_inspection_stops_at_its_budget_and_leaves_the_rest_unanswered() {
    let temp = tempfile::tempdir().unwrap();
    let sources: Vec<_> = (0..6)
        .map(|_| source(temp.path(), uuid::Uuid::now_v7()))
        .collect();
    // Two at a time for 2.5 s: two rounds of two complete.
    let verdicts =
        inspect_in_background(&SlowInspector, sources.clone(), Duration::from_millis(2500)).await;
    let present = sources
        .iter()
        .filter(|source| matches!(verdicts.check(source), RetainedSourceCheck::Present { .. }))
        .count();
    assert_eq!(present, 4);
    // The rest are unresolved for this batch, never assumed present or absent.
    assert_eq!(verdicts.take_unresolved().len(), 2);
}

#[test]
fn pending_sources_keep_their_age_across_pages_until_answered() {
    let temp = tempfile::tempdir().unwrap();
    let a = source(temp.path(), uuid::Uuid::now_v7());
    let b = source(temp.path(), uuid::Uuid::now_v7());
    let unresolved = |sources: &[&RetainedSource]| {
        let verdicts = RetainedSourceVerdicts::default();
        for source in sources {
            verdicts.check(source);
        }
        verdicts
    };
    let start = Instant::now();
    let mut pending = PendingRetainedSources::default();
    pending.observe(&[&unresolved(&[&a])], start);
    // A page without `a` neither clears nor resets it.
    pending.observe(&[&unresolved(&[&b])], start + Duration::from_secs(60));
    assert_eq!(pending.seen.get(&a).map(|(first, _)| *first), Some(start));
    assert!(pending.seen.contains_key(&b));
    // An answer clears it.
    let mut answered = RetainedSourceVerdicts::default();
    answered.record(a.clone(), RetainedSourceCheck::Absent);
    pending.observe(&[&answered], start + Duration::from_secs(120));
    assert!(!pending.seen.contains_key(&a));
    // A source nobody asks about any more stops counting.
    pending.observe(&[], start + Duration::from_secs(60) + PENDING_FORGET_AFTER);
    assert!(pending.seen.is_empty());
}
