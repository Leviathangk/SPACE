//! Artifact-owned immutable send inputs. A private parent snapshot keeps retries
//! independent of template GC; per-attempt wire clones avoid exporting an UUID
//! that a same-filesystem receive could still find in the retained source.
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use pplx_space_common_svc::{LocalRole, UdsPeerPrincipal};
use tonic::{Request, Response, Status};
use uuid::Uuid;

use super::{NodeManagerService, artifact_fs, trusted_paths};
use crate::btrfs;
use crate::proto::{BtrfsEncodeRootfsSendResponse, BtrfsMaterializeRootfsSendRequest};

pub(super) const DIRECTORY: &str = "rootfs.btrfs-source";
const MANIFEST: &str = "source.json";
const ALLOWED: &[UdsPeerPrincipal] = &[
    UdsPeerPrincipal::Root,
    UdsPeerPrincipal::Role(LocalRole::Admin),
    UdsPeerPrincipal::Service(
        pplx_space_common_core::host::linux::identities::user_names::SPACE_SBXMGR,
    ),
];

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    source_id: Uuid,
    source_uuid: Uuid,
    parent_uuid: Uuid,
    received_uuid: Uuid,
}

/// The caller owns the enclosing artifact until publication or cleanup. No
/// pathname is reopened after pinning it, including when a cache peer renames
/// the enclosing directory. A partial capture remains inside that same owner.
pub(super) fn capture(artifact: &Path, parent: &Path, source: &Path) -> Result<Uuid, Status> {
    let artifact = trusted_paths::open_directory(artifact)?;
    let parent = trusted_paths::open_directory(parent)?;
    let source = trusted_paths::open_directory(source)?;
    capture_open(&artifact, &parent, &source, Uuid::now_v7())
}

pub(super) fn capture_open(
    artifact: &File,
    parent: &File,
    source: &File,
    source_id: Uuid,
) -> Result<Uuid, Status> {
    let received_uuid = btrfs::info(parent)
        .map_err(artifact_fs::error)?
        .1
        .received_uuid
        .ok_or_else(|| Status::failed_precondition("send parent has no received identity"))?;
    nix::sys::stat::mkdirat(
        artifact,
        DIRECTORY,
        nix::sys::stat::Mode::from_bits_truncate(0o750),
    )
    .map_err(artifact_fs::error)?;
    let bundle =
        artifact_fs::directory(artifact, DIRECTORY.as_ref()).map_err(artifact_fs::error)?;
    trusted_paths::set_permissions(
        &bundle,
        Some(pplx_space_common_core::host::linux::identities::gids::SPACE_CACHE_RW),
        0o750,
    )?;
    let result = (|| {
        // Templates use a deterministic received UUID and stransid=0. Preserve
        // that remote identity on the private, byte-identical parent before
        // sealing it. Never change the original template's identity or flags.
        btrfs::snapshot(parent, &bundle, "parent".as_ref(), false).map_err(artifact_fs::error)?;
        let private_parent =
            artifact_fs::directory(&bundle, "parent".as_ref()).map_err(artifact_fs::error)?;
        btrfs::stamp(&private_parent, received_uuid).map_err(artifact_fs::error)?;
        btrfs::set_readonly(&private_parent, true).map_err(artifact_fs::error)?;
        btrfs::snapshot(source, &bundle, "source".as_ref(), true).map_err(artifact_fs::error)?;
        let private_source =
            artifact_fs::directory(&bundle, "source".as_ref()).map_err(artifact_fs::error)?;
        let manifest = Manifest {
            version: 1,
            source_id,
            source_uuid: subvolume_uuid(&private_source)?,
            parent_uuid: subvolume_uuid(&private_parent)?,
            received_uuid,
        };
        let bytes = serde_json::to_vec(&manifest).map_err(artifact_fs::error)?;
        let mut file = File::from(
            nix::fcntl::openat(
                &bundle,
                MANIFEST,
                nix::fcntl::OFlag::O_WRONLY
                    | nix::fcntl::OFlag::O_CREAT
                    | nix::fcntl::OFlag::O_EXCL
                    | nix::fcntl::OFlag::O_NOFOLLOW
                    | nix::fcntl::OFlag::O_CLOEXEC,
                nix::sys::stat::Mode::from_bits_truncate(0o640),
            )
            .map_err(artifact_fs::error)?,
        );
        file.write_all(&bytes).map_err(artifact_fs::error)?;
        trusted_paths::set_permissions(
            &file,
            Some(pplx_space_common_core::host::linux::identities::gids::SPACE_CACHE_RW),
            0o640,
        )?;
        file.sync_all().map_err(artifact_fs::error)?;
        bundle.sync_all().map_err(artifact_fs::error)?;
        artifact.sync_all().map_err(artifact_fs::error)?;
        Ok(manifest.source_id)
    })();
    if result.is_err()
        && let Err(error) = artifact_fs::remove_at(artifact, DIRECTORY.as_ref())
    {
        tracing::warn!(%error, "partial send-source cleanup deferred to artifact cleanup");
    }
    result
}

fn subvolume_uuid(file: &File) -> Result<Uuid, Status> {
    btrfs::info(file)
        .map_err(artifact_fs::error)?
        .1
        .uuid
        .ok_or_else(|| Status::failed_precondition("send source has no subvolume identity"))
}

pub(super) struct SendSource {
    bundle: File,
    pub(super) parent: File,
    pub(super) source: File,
}

impl SendSource {
    pub(super) fn open(artifact: &File, expected: Uuid) -> Result<Self, Status> {
        let bundle =
            artifact_fs::directory(artifact, DIRECTORY.as_ref()).map_err(artifact_fs::error)?;
        let mut bytes = Vec::new();
        trusted_paths::open_regular_file(&bundle, MANIFEST.as_ref())?
            .take(4097)
            .read_to_end(&mut bytes)
            .map_err(artifact_fs::error)?;
        if bytes.len() > 4096 {
            return Err(Status::failed_precondition(
                "oversized send source manifest",
            ));
        }
        let manifest: Manifest = serde_json::from_slice(&bytes).map_err(artifact_fs::error)?;
        if manifest.version != 1 || manifest.source_id != expected {
            return Err(Status::failed_precondition("send source identity changed"));
        }
        let parent =
            artifact_fs::directory(&bundle, "parent".as_ref()).map_err(artifact_fs::error)?;
        let source =
            artifact_fs::directory(&bundle, "source".as_ref()).map_err(artifact_fs::error)?;
        if subvolume_uuid(&parent)? != manifest.parent_uuid
            || subvolume_uuid(&source)? != manifest.source_uuid
            || btrfs::info(&parent)
                .map_err(artifact_fs::error)?
                .1
                .received_uuid
                != Some(manifest.received_uuid)
            || !btrfs::is_readonly(&parent).map_err(artifact_fs::error)?
            || !btrfs::is_readonly(&source).map_err(artifact_fs::error)?
        {
            return Err(Status::failed_precondition(
                "retained send source or parent changed",
            ));
        }
        Ok(Self {
            bundle,
            parent,
            source,
        })
    }

    pub(super) fn send(&self, output: &File) -> Result<(), Status> {
        let name = format!(".send-{}", Uuid::now_v7());
        nix::sys::stat::mkdirat(
            &self.bundle,
            name.as_str(),
            nix::sys::stat::Mode::from_bits_truncate(0o700),
        )
        .map_err(artifact_fs::error)?;
        let result = (|| {
            let attempt =
                artifact_fs::directory(&self.bundle, name.as_ref()).map_err(artifact_fs::error)?;
            btrfs::snapshot(&self.source, &attempt, "instance.ro".as_ref(), true)
                .map_err(artifact_fs::error)?;
            let wire = artifact_fs::directory(&attempt, "instance.ro".as_ref())
                .map_err(artifact_fs::error)?;
            btrfs::send(&self.parent, &wire, output).map_err(artifact_fs::error)
        })();
        // Kernel send has returned before releasing the wire clone, even if
        // the RPC waiter disappeared. Artifact cleanup owns interrupted attempts.
        let cleanup = artifact_fs::remove_at(&self.bundle, name.as_ref());
        if let Err(error) = cleanup {
            tracing::warn!(%error, "send wire-clone cleanup deferred to artifact cleanup");
            return result.and(Err(error));
        }
        result
    }
}

pub(super) fn materialize(
    artifact: &Path,
    expected: Uuid,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<u64, Status> {
    let directory = trusted_paths::open_directory(artifact)?;
    let source = SendSource::open(&directory, expected)?;
    let output =
        artifact_fs::write_atomic_at(&directory, "rootfs.btrfs-send".as_ref(), |output| {
            source.send(output)?;
            if cancel.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
                return Err(Status::cancelled("send source materialization cancelled"));
            }
            trusted_paths::set_permissions(
                output,
                Some(pplx_space_common_core::host::linux::identities::gids::SPACE_CACHE_RW),
                0o660,
            )
        })?;
    output
        .metadata()
        .map(|m| m.len())
        .map_err(artifact_fs::error)
}

impl NodeManagerService {
    pub(super) async fn btrfs_materialize_rootfs_send(
        &self,
        request: Request<BtrfsMaterializeRootfsSendRequest>,
    ) -> Result<Response<BtrfsEncodeRootfsSendResponse>, Status> {
        self.authorize_peer(&request, "BtrfsMaterializeRootfsSend", ALLOWED)?;
        let req = request.into_inner();
        let artifact = crate::runtime_paths::validate_create_snapshot_dest(
            &req.sandbox_id,
            &req.artifact_dir,
        )?;
        let source_id = req
            .source_id
            .parse::<Uuid>()
            .map_err(|_| Status::invalid_argument("invalid send source identity"))?;
        self.run_artifact_mutation(&req.sandbox_id, move |service| async move {
            let owner = service.artifact_work.clone();
            let stream_bytes = service
                .off_runtime("regenerate rootfs send", move || {
                    materialize(
                        &artifact,
                        source_id,
                        owner.as_ref().map(|owner| &owner.cancel),
                    )
                })
                .await?;
            Ok(Response::new(BtrfsEncodeRootfsSendResponse {
                stream_bytes,
                source_id: Some(source_id.to_string()),
            }))
        })
        .await
    }
}

#[cfg(test)]
mod tests;
