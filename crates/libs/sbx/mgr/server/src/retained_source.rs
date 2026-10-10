//! Retained btrfs send sources from deferred capture.
//!
//! Deferred capture skips `rootfs.btrfs-send` and leaves an immutable source
//! bundle under `rootfs.btrfs-source/` in the artifact directory. The bundle's
//! source subvolume keeps the sandbox jail's ownership, which this service
//! cannot traverse. Node-manager is therefore the only authority over a
//! retained source. Sandbox-manager knows a source by the identity node-manager
//! returned at capture, persisted as `snapshot.json` `rootfs_send_source_id`,
//! and asks node-manager whether it is still usable. It never opens the bundle.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use futures_util::StreamExt as _;

use crate::snapshot_upload::{ROOTFS_BTRFS_SEND_FILENAME, SnapshotMeta};

/// Node-manager's bundle directory. Named in errors only; never opened here.
pub(crate) const BUNDLE_DIRECTORY: &str = "rootfs.btrfs-source";

/// Per-call deadline for one inspection.
const INSPECTION_DEADLINE: Duration = Duration::from_secs(2);
/// Background inspections in flight per node.
const BACKGROUND_INSPECTIONS: usize = 2;
/// Inspection time a background pass may spend before deferring the rest.
pub(crate) const BACKGROUND_INSPECTION_BUDGET: Duration = Duration::from_secs(10);

/// A retained source as sandbox-manager knows it: which sandbox and artifact
/// directory hold it, and its identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct RetainedSource {
    pub(crate) sandbox_id: String,
    pub(crate) artifact_dir: PathBuf,
    pub(crate) source_id: uuid::Uuid,
}

/// The btrfs-send rootfs of a local artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RootfsArtifact {
    /// A materialized `rootfs.btrfs-send` stream.
    SendFile { path: PathBuf },
    /// Deferred capture: no stream exists, only node-manager's source.
    RetainedSource(RetainedSource),
}

impl RootfsArtifact {
    /// A stream file wins. Otherwise the persisted identity names a retained
    /// source. Without either, the btrfs-send rootfs is missing. Reads no
    /// path under the bundle.
    pub(crate) fn resolve(
        sandbox_id: &str,
        artifact_dir: &Path,
        source_id: Option<uuid::Uuid>,
    ) -> Result<Option<Self>, RootfsResolveError> {
        let path = artifact_dir.join(ROOTFS_BTRFS_SEND_FILENAME);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => Ok(Some(Self::SendFile { path })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(source_id.map(|source_id| {
                    Self::RetainedSource(RetainedSource {
                        sandbox_id: sandbox_id.to_owned(),
                        artifact_dir: artifact_dir.to_owned(),
                        source_id,
                    })
                }))
            }
            Err(source) => Err(RootfsResolveError { path, source }),
        }
    }

    pub(crate) fn resolve_meta(
        meta: &SnapshotMeta,
        artifact_dir: &Path,
    ) -> Result<Option<Self>, RootfsResolveError> {
        Self::resolve(&meta.sandbox_id, artifact_dir, meta.rootfs_send_source_id)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("stat {path} failed: {source}")]
pub(crate) struct RootfsResolveError {
    pub(crate) path: PathBuf,
    #[source]
    pub(crate) source: std::io::Error,
}

/// Node-manager's answer about one retained source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RetainedSourceCheck {
    /// Usable. The length is the retained rootfs image, never a send stream.
    Present { rootfs_bytes: u64 },
    /// The bundle is gone.
    Absent,
    /// The bundle exists but cannot be used: its identity, lineage, or
    /// read-only state changed, or its rootfs is missing.
    Invalid { reason: String },
    /// No answer, including from a node-manager without inspection. Never
    /// evidence about the artifact.
    Unavailable { reason: String },
}

impl RetainedSourceCheck {
    pub(crate) fn from_status(status: &tonic::Status) -> Self {
        match status.code() {
            tonic::Code::NotFound => Self::Absent,
            tonic::Code::FailedPrecondition => Self::Invalid {
                reason: status.message().to_owned(),
            },
            _ => Self::Unavailable {
                reason: status.to_string(),
            },
        }
    }

    fn record_metric(&self, code: Option<tonic::Code>) {
        let outcome = match self {
            Self::Present { .. } => "present",
            Self::Absent => "absent",
            Self::Invalid { .. } => "invalid",
            Self::Unavailable { .. } => "unavailable",
        };
        pplx_space_metrics::snapshot::retained_source_inspection(
            outcome,
            code.map(code_tag)
                .filter(|_| matches!(self, Self::Unavailable { .. })),
        );
    }
}

impl std::fmt::Display for RetainedSourceCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Present { rootfs_bytes } => write!(f, "present ({rootfs_bytes} rootfs bytes)"),
            Self::Absent => f.write_str("absent"),
            Self::Invalid { reason } => write!(f, "unusable: {reason}"),
            Self::Unavailable { reason } => write!(f, "unverified: {reason}"),
        }
    }
}

fn code_tag(code: tonic::Code) -> &'static str {
    match code {
        tonic::Code::Unimplemented => "unimplemented",
        tonic::Code::Unavailable => "unavailable",
        tonic::Code::DeadlineExceeded => "deadline_exceeded",
        tonic::Code::ResourceExhausted => "resource_exhausted",
        tonic::Code::PermissionDenied => "permission_denied",
        tonic::Code::Unauthenticated => "unauthenticated",
        tonic::Code::Internal => "internal",
        _ => "other",
    }
}

/// Asks node-manager about retained sources. Injected so tests can substitute
/// verdicts without a btrfs node-manager.
#[async_trait::async_trait]
pub(crate) trait RetainedSourceInspector: Send + Sync {
    async fn inspect(&self, source: &RetainedSource) -> RetainedSourceCheck;
}

/// One node-manager call with a bounded deadline.
pub(crate) struct NodeManagerSourceInspector;

#[async_trait::async_trait]
impl RetainedSourceInspector for NodeManagerSourceInspector {
    async fn inspect(&self, source: &RetainedSource) -> RetainedSourceCheck {
        let call = crate::btrfs_via_nodemgr::inspect_send_source(
            &source.sandbox_id,
            &source.artifact_dir,
            source.source_id,
        );
        let (check, code) = match tokio::time::timeout(INSPECTION_DEADLINE, call).await {
            Ok(Ok(rootfs_bytes)) => (RetainedSourceCheck::Present { rootfs_bytes }, None),
            Ok(Err(error)) => {
                let status = error.into_status();
                (
                    RetainedSourceCheck::from_status(&status),
                    Some(status.code()),
                )
            }
            Err(_) => (
                RetainedSourceCheck::Unavailable {
                    reason: format!(
                        "node-manager inspection exceeded {}s",
                        INSPECTION_DEADLINE.as_secs()
                    ),
                },
                Some(tonic::Code::DeadlineExceeded),
            ),
        };
        check.record_metric(code);
        if let RetainedSourceCheck::Unavailable { reason } = &check {
            tracing::debug!(
                sandbox_id = %source.sandbox_id,
                artifact_dir = %source.artifact_dir.display(),
                source_id = %source.source_id,
                reason,
                "retained send source inspection unavailable"
            );
        }
        check
    }
}

/// Verdicts gathered by an async caller for synchronous validation, which
/// must not wait on node-manager. A source without a verdict is pending:
/// neither complete nor evidence of loss. Validation records every source it
/// could not resolve, so its owner can inspect them before the next attempt.
#[derive(Debug, Default)]
pub(crate) struct RetainedSourceVerdicts {
    verdicts: HashMap<RetainedSource, RetainedSourceCheck>,
    unresolved: Mutex<BTreeSet<RetainedSource>>,
}

impl RetainedSourceVerdicts {
    pub(crate) fn record(&mut self, source: RetainedSource, check: RetainedSourceCheck) {
        self.verdicts.insert(source, check);
    }

    /// The verdict for `source`, or `Unavailable` when none was gathered.
    pub(crate) fn check(&self, source: &RetainedSource) -> RetainedSourceCheck {
        let check = self.verdicts.get(source).cloned().unwrap_or_else(|| {
            RetainedSourceCheck::Unavailable {
                reason: "awaiting node-manager inspection".to_owned(),
            }
        });
        if matches!(check, RetainedSourceCheck::Unavailable { .. }) {
            self.unresolved
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(source.clone());
        }
        check
    }

    /// Sources a validation could not resolve since the last call.
    pub(crate) fn take_unresolved(&self) -> Vec<RetainedSource> {
        std::mem::take(
            &mut *self
                .unresolved
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
        .into_iter()
        .collect()
    }
}

/// Inspect `sources` in the background lane: bounded concurrency, and at most
/// `budget` before the rest wait for a later pass. `cursor` rotates the start
/// across passes, so slow sources cannot starve the others.
pub(crate) async fn inspect_in_background(
    inspector: &dyn RetainedSourceInspector,
    sources: impl IntoIterator<Item = RetainedSource>,
    budget: Duration,
    cursor: &mut Option<RetainedSource>,
) -> RetainedSourceVerdicts {
    let sources: BTreeSet<_> = sources.into_iter().collect();
    let (before, after): (Vec<_>, Vec<_>) = sources
        .into_iter()
        .partition(|source| cursor.as_ref().is_some_and(|cursor| source <= cursor));
    let deadline = tokio::time::Instant::now() + budget;
    let mut inspections = futures_util::stream::iter(after.into_iter().chain(before))
        .map(|source| async move {
            let check = inspector.inspect(&source).await;
            (source, check)
        })
        .buffered(BACKGROUND_INSPECTIONS);
    let mut verdicts = RetainedSourceVerdicts::default();
    while let Ok(Some((source, check))) =
        tokio::time::timeout_at(deadline, inspections.next()).await
    {
        *cursor = Some(source.clone());
        verdicts.record(source, check);
    }
    verdicts
}

/// Unresolved sources across periodic passes, for the pending signals.
#[derive(Debug, Default)]
pub(crate) struct PendingRetainedSources {
    since: BTreeMap<RetainedSource, Instant>,
}

impl PendingRetainedSources {
    /// Replace the pending set with the latest pass's unresolved sources.
    pub(crate) fn observe(&mut self, unresolved: &[RetainedSource], now: Instant) {
        let unresolved: BTreeSet<_> = unresolved.iter().collect();
        self.since.retain(|source, _| unresolved.contains(source));
        for source in unresolved {
            self.since.entry(source.clone()).or_insert(now);
        }
        let oldest = self
            .since
            .values()
            .map(|since| now.saturating_duration_since(*since))
            .max()
            .unwrap_or_default();
        pplx_space_metrics::snapshot::retained_source_pending(
            self.since.len(),
            oldest.as_secs_f64(),
        );
    }
}

/// Answers every inspection with one configurable verdict and records calls.
#[cfg(test)]
pub(crate) struct FakeInspector {
    answer: Mutex<RetainedSourceCheck>,
    calls: Mutex<Vec<RetainedSource>>,
}

#[cfg(test)]
impl FakeInspector {
    pub(crate) fn new(answer: RetainedSourceCheck) -> Self {
        Self {
            answer: Mutex::new(answer),
            calls: Mutex::default(),
        }
    }

    pub(crate) fn answer(&self, answer: RetainedSourceCheck) {
        *self
            .answer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = answer;
    }

    pub(crate) fn calls(&self) -> Vec<RetainedSource> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl RetainedSourceInspector for FakeInspector {
    async fn inspect(&self, source: &RetainedSource) -> RetainedSourceCheck {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(source.clone());
        self.answer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
mod tests;
