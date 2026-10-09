//! Privileged btrfs operations, delegated to node-manager.
//!
//! Sbxmgr runs as a non-root user with `runAsNonRoot: true` + no
//! `capabilities.add` in production — it cannot issue the
//! `BTRFS_IOC_SEND` / `BTRFS_IOC_RECEIVE` / `BTRFS_IOC_SNAP_DESTROY_V2`
//! ioctls the suspend codec depends on. Node-manager runs as the
//! privileged daemon and exposes three gRPC RPCs for the
//! suspend-codec lifecycle: encode, receive, delete.
//!
//! These helpers wrap the per-call gRPC dial + map errors. Every
//! call takes domain identifiers (`sandbox_id`, `template_name`,
//! `content_hash`); nodemgr derives canonical host paths internally
//! so callers can't smuggle arbitrary paths.

use pplx_space_node_mgr_client::execution_fence::{ExecutionFence, InstanceTarget};
use pplx_space_node_mgr_client::proto::{
    BtrfsDeleteSandboxInstanceRequest, BtrfsEncodeRootfsSendRequest,
    BtrfsEncodeSnapshotRootfsSendRequest, BtrfsReceiveInstanceRequest,
};
use thiserror::Error;
use tonic::Status;

// `pub` only because `RestoreError::BtrfsReceive` carries it and `RestoreError`
// is public; the module is `pub(crate)`, so the type is still not exported.
#[derive(Debug, Error)]
pub enum BtrfsViaNodemgrError {
    #[error("connect to node-manager: {0}")]
    Connect(String),
    #[error("node-manager rpc {rpc} failed: {status}")]
    Rpc {
        rpc: &'static str,
        #[source]
        status: tonic::Status,
    },
}

impl BtrfsViaNodemgrError {
    /// Convert the error back to a `tonic::Status` for propagation
    /// to sbxmgr's gRPC callers. Preserves nodemgr's
    /// `FailedPrecondition` codes (used for codec precondition
    /// mismatches the caller can retry with format=full) and
    /// collapses transport / unspecified failures to `Internal`.
    pub(crate) fn into_status(self) -> Status {
        match self {
            BtrfsViaNodemgrError::Connect(reason) => {
                Status::internal(format!("node-manager unreachable: {reason}"))
            }
            BtrfsViaNodemgrError::Rpc { rpc: _, status } => status,
        }
    }
}

/// Encode a sandbox's rootfs as a `btrfs send -p` stream via
/// node-manager. The output stream lands in `snapshot_dir`; nodemgr also
/// cleans up the intermediate `instance.ro` subvolume on success.
pub(crate) async fn encode_rootfs_send(
    sandbox_id: &str,
    template_name: &str,
    content_hash: &str,
    snapshot_dir: &str,
    execution_fence: ExecutionFence,
) -> Result<u64, BtrfsViaNodemgrError> {
    let mut client = pplx_space_node_mgr_client::connect()
        .await
        .map_err(|e| BtrfsViaNodemgrError::Connect(e.to_string()))?;
    let response = client
        .btrfs_encode_rootfs_send(BtrfsEncodeRootfsSendRequest {
            sandbox_id: sandbox_id.to_string(),
            template_name: template_name.to_string(),
            content_hash: content_hash.to_string(),
            dest_dir: Some(snapshot_dir.to_string()),
            execution_fence: Some(execution_fence.into()),
            retain_source: retain_send_sources(),
            defer_send: defer_send().await,
        })
        .await
        .map_err(|status| BtrfsViaNodemgrError::Rpc {
            rpc: "BtrfsEncodeRootfsSend",
            status,
        })?
        .into_inner();
    Ok(response.stream_bytes)
}

/// Encode an already-created immutable named snapshot. This is distinct from
/// [`encode_rootfs_send`], which captures the live instance itself.
pub(crate) async fn encode_snapshot_rootfs_send(
    sandbox_id: &str,
    template_name: &str,
    content_hash: &str,
    snapshot_name: &str,
    dest_dir: &str,
) -> Result<u64, BtrfsViaNodemgrError> {
    let mut client = pplx_space_node_mgr_client::connect()
        .await
        .map_err(|e| BtrfsViaNodemgrError::Connect(e.to_string()))?;
    let response = client
        .btrfs_encode_snapshot_rootfs_send(BtrfsEncodeSnapshotRootfsSendRequest {
            sandbox_id: sandbox_id.to_string(),
            template_name: template_name.to_string(),
            content_hash: content_hash.to_string(),
            snapshot_name: snapshot_name.to_string(),
            dest_dir: dest_dir.to_string(),
            retain_source: retain_send_sources(),
            defer_send: defer_send().await,
        })
        .await
        .map_err(|status| BtrfsViaNodemgrError::Rpc {
            rpc: "BtrfsEncodeSnapshotRootfsSend",
            status,
        })?
        .into_inner();
    Ok(response.stream_bytes)
}

/// Consume a `rootfs.btrfs-send` stream from
/// `<sandbox_dir>/snapshot/rootfs.btrfs-send` and reconstruct the
/// sandbox's `instance/` subvolume against the local template via
/// `btrfs receive`. Nodemgr also cleans up stale `instance` /
/// `instance.ro` from prior failed attempts, renames the received
/// subvolume to `instance/`, and toggles RW so FC can write.
pub(crate) async fn receive_instance(
    sandbox_id: &str,
    template_name: &str,
    content_hash: &str,
    target: InstanceTarget,
) -> Result<(), BtrfsViaNodemgrError> {
    let mut client = pplx_space_node_mgr_client::connect()
        .await
        .map_err(|e| BtrfsViaNodemgrError::Connect(e.to_string()))?;
    client
        .btrfs_receive_instance(BtrfsReceiveInstanceRequest {
            sandbox_id: sandbox_id.to_string(),
            template_name: template_name.to_string(),
            content_hash: content_hash.to_string(),
            target: Some(target.into()),
        })
        .await
        .map_err(|status| BtrfsViaNodemgrError::Rpc {
            rpc: "BtrfsReceiveInstance",
            status,
        })?;
    Ok(())
}

/// Subvolume-aware delete of a sandbox's `instance/` (and any stray
/// `instance.ro` from a crashed prior encode). Used by failure-path
/// cleanups in the restore dispatcher.
pub(crate) async fn delete_sandbox_instance(
    sandbox_id: &str,
    target: InstanceTarget,
) -> Result<(), BtrfsViaNodemgrError> {
    let mut client = pplx_space_node_mgr_client::connect()
        .await
        .map_err(|e| BtrfsViaNodemgrError::Connect(e.to_string()))?;
    client
        .btrfs_delete_sandbox_instance(BtrfsDeleteSandboxInstanceRequest {
            sandbox_id: sandbox_id.to_string(),
            target: Some(target.into()),
        })
        .await
        .map_err(|status| BtrfsViaNodemgrError::Rpc {
            rpc: "BtrfsDeleteSandboxInstance",
            status,
        })?;
    Ok(())
}

/// Capture policy is separate from recovery: disabling new captures must not
/// prevent an already-published source from being regenerated.
fn retain_send_sources() -> bool {
    static ENABLED: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var("SPACE_SNAPSHOT_STREAM_BTRFS_SEND").is_ok_and(|value| value == "true")
    });
    *ENABLED
}

pub(crate) async fn materialize_rootfs_send(
    sandbox_id: &str,
    artifact_dir: &std::path::Path,
    source_id: uuid::Uuid,
) -> Result<u64, BtrfsViaNodemgrError> {
    let mut client = pplx_space_node_mgr_client::connect()
        .await
        .map_err(|e| BtrfsViaNodemgrError::Connect(e.to_string()))?;
    client
        .btrfs_materialize_rootfs_send(
            pplx_space_node_mgr_client::proto::BtrfsMaterializeRootfsSendRequest {
                sandbox_id: sandbox_id.to_owned(),
                artifact_dir: artifact_dir.to_string_lossy().into_owned(),
                source_id: source_id.to_string(),
            },
        )
        .await
        .map(|response| response.into_inner().stream_bytes)
        .map_err(|status| BtrfsViaNodemgrError::Rpc {
            rpc: "BtrfsMaterializeRootfsSend",
            status,
        })
}

async fn defer_send() -> bool {
    if !retain_send_sources() {
        return false;
    }
    let probe = async {
        let mut client = pplx_space_volmgr_client::connect(pplx_space_volmgr_client::VOLMGR_SOCKET)
            .await
            .map_err(|error| tonic::Status::unavailable(error.to_string()))?;
        client
            .get_push_capabilities(pplx_space_volmgr_client::proto::GetPushCapabilitiesRequest {})
            .await
            .map(|response| response.into_inner().btrfs_send)
    };
    match tokio::time::timeout(std::time::Duration::from_secs(2), probe).await {
        Ok(Ok(enabled)) => enabled,
        result => {
            tracing::debug!(
                ?result,
                "using materialized send without streaming capability"
            );
            false
        }
    }
}
