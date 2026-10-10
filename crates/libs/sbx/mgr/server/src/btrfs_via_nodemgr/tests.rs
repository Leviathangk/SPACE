use super::*;

const RPC: &str = "BtrfsEncodeRootfsSend";

#[test]
fn capture_response_proves_deferral_only_with_identity_size_and_no_stream() {
    let id = uuid::Uuid::now_v7();
    let id_text = id.to_string();
    let source = Some(id_text.as_str());

    assert_eq!(
        CapturedRootfs::from_response(RPC, true, 0, source, Some(42)).unwrap(),
        CapturedRootfs::Deferred {
            source_id: id,
            rootfs_bytes: 42
        }
    );
    // Node-manager may materialize despite the request; the stream wins and
    // the retained source still names the rootfs.
    assert_eq!(
        CapturedRootfs::from_response(RPC, true, 9, source, Some(42)).unwrap(),
        CapturedRootfs::Stream {
            stream_bytes: 9,
            retained: Some((id, 42))
        }
    );
    // Without retention there is nothing to defer to.
    assert_eq!(
        CapturedRootfs::from_response(RPC, true, 9, None, None).unwrap(),
        CapturedRootfs::Stream {
            stream_bytes: 9,
            retained: None
        }
    );
    assert_eq!(
        CapturedRootfs::from_response(RPC, false, 9, source, Some(42)).unwrap(),
        CapturedRootfs::Stream {
            stream_bytes: 9,
            retained: Some((id, 42))
        }
    );
}

#[test]
fn capture_response_without_retained_size_fails_instead_of_sizing_zero() {
    let id = uuid::Uuid::now_v7().to_string();
    // The local artifact counts the retained image, so a materialized capture
    // that retained a source must size it too.
    assert!(matches!(
        CapturedRootfs::from_response(RPC, false, 9, Some(&id), None),
        Err(BtrfsViaNodemgrError::InvalidResponse { .. })
    ));
    let error = CapturedRootfs::from_response(RPC, true, 0, Some(&id), None).unwrap_err();
    assert!(matches!(
        error,
        BtrfsViaNodemgrError::InvalidResponse { .. }
    ));
    // Not FailedPrecondition: rootfs.ext4 is already gone, so a Full
    // fallback cannot succeed.
    assert_eq!(error.into_status().code(), tonic::Code::Internal);
    assert!(matches!(
        CapturedRootfs::from_response(RPC, true, 0, Some("not-a-uuid"), Some(1)),
        Err(BtrfsViaNodemgrError::InvalidResponse { .. })
    ));
}
