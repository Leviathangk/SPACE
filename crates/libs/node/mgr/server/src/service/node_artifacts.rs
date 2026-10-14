//! The node's artifact store: publishing runtime releases and guest kernels
//! the runtime manager staged, and removing them.
//!
//! Node-manager is the store's only writer and exposes the compiled E0 binding
//! plus explicitly admitted experiments
//! to execution. It decides what gets in: it loads the catalog's pins
//! itself for every request and publishes only bytes that match them (see
//! [`pplx_space_node_runtime_core::artifact_store`]). The runtime manager
//! names an artifact by what it is, never by a path. A catalog that cannot
//! be loaded safely refuses the request, not node-manager's startup.

use pplx_space_common_core::types::{Arch, ArtifactVersion, RuntimeIdentity, RuntimeVersion};
use pplx_space_common_svc::UdsPeerPrincipal;
use pplx_space_common_svc::core::host::linux::identities::user_names;
use pplx_space_node_runtime_core::artifact_store::{
    ArtifactStore, Publication, PublishError, PublishOutcome, RemoveError, RemoveOutcome,
    StagedName,
};
use pplx_space_node_runtime_core::catalog::{InitialEnvironment, RuntimeCatalog};
use tonic::{Request, Response, Status};

use crate::proto::{
    NodeArtifact, PublishArtifactRequest, PublishArtifactResponse, RemoveArtifactRequest,
    RemoveArtifactResponse, node_artifact,
};

use super::NodeManagerService;

use pplx_space_node_runtime_core::runtime_inventory::{
    ExecutionSelectionMode, ResolvedExecutionEnvironment,
};

impl NodeManagerService {
    fn allowed_definition(
        &self,
        reference: Option<pplx_space_common_core::types::EnvironmentReference>,
        mode: ExecutionSelectionMode,
    ) -> Result<pplx_space_execution_common::environment::EnvironmentDefinition, Status> {
        select_definition(
            initial_environment()?,
            if self.experiment_policy.retains() {
                benchmark_environment()?
            } else {
                None
            },
            self.experiment_policy.admits(),
            reference,
            mode,
        )
    }

    async fn require_verified_environment_layout(&self) -> Result<(), Status> {
        if !cfg!(target_os = "linux") {
            return Err(Status::failed_precondition(
                "the compiled E0 environment requires a Linux host",
            ));
        }
        let paths = self.runtime_paths.clone();
        self.off_runtime("verify environment layout", move || {
            separate_store(&paths).map_err(|error| Status::failed_precondition(describe(&error)))
        })
        .await
    }

    pub async fn resolve_execution_environment(
        &self,
        request: Request<crate::proto::ResolveExecutionEnvironmentRequest>,
    ) -> Result<Response<crate::proto::ResolveExecutionEnvironmentResponse>, Status> {
        self.authorize_peer(
            &request,
            "ResolveExecutionEnvironment",
            super::ALLOW_NODE_STATUS,
        )?;
        self.require_verified_environment_layout().await?;
        let reference = request
            .into_inner()
            .reference
            .map(|reference| reference.parse())
            .transpose()
            .map_err(|error: pplx_space_common_core::types::DigestParseError| {
                Status::invalid_argument(error.to_string())
            })?;
        let definition = self.allowed_definition(reference, ExecutionSelectionMode::NewColdBoot)?;
        let requested = definition
            .execution_requirements()
            .map_err(|error| Status::failed_precondition(error.to_string()))?;
        let inventory = self.runtime_inventory().await?;
        let resolved = inventory
            .resolve(
                Some(&requested),
                requested.runtime().kind,
                ExecutionSelectionMode::NewColdBoot,
            )
            .map_err(super::RuntimeInventoryError::into_status)?;
        let execution = definition
            .bind_execution(resolved.execution)
            .map_err(|error| Status::failed_precondition(describe(&error)))?;
        Ok(Response::new(
            crate::proto::ResolveExecutionEnvironmentResponse {
                definition: Some((&definition).into()),
                execution: Some((&execution).into()),
            },
        ))
    }

    pub(super) async fn resolve_bound_execution(
        &self,
        requested: Option<&pplx_space_common_core::types::ExecutionMetadata>,
        runtime: pplx_space_common_core::types::RuntimeKind,
        mode: ExecutionSelectionMode,
    ) -> Result<ResolvedExecutionEnvironment, Status> {
        let definition =
            if let Some(reference) = requested.and_then(|metadata| metadata.environment()) {
                self.require_verified_environment_layout().await?;
                let definition = self.allowed_definition(Some(reference), mode)?;
                if let Some(requested) = requested {
                    definition
                        .bind_execution(requested.clone())
                        .map_err(|error| Status::failed_precondition(describe(&error)))?;
                }
                Some(definition)
            } else {
                None
            };
        let inventory = self.runtime_inventory().await?;
        let mut resolved = inventory
            .resolve(requested, runtime, mode)
            .map_err(super::RuntimeInventoryError::into_status)?;
        if definition.is_none()
            && self.experiment_policy.retains()
            && benchmark_environment()?.is_some_and(|experiment| {
                resolved.execution.runtime() == &experiment.release().release
            })
        {
            return Err(Status::failed_precondition(
                "experimental runtime requires an explicit environment binding",
            ));
        }
        if let Some(definition) = definition {
            resolved.execution = definition
                .bind_execution(resolved.execution)
                .map_err(|error| Status::failed_precondition(describe(&error)))?;
        }
        Ok(resolved)
    }
}

fn select_definition(
    initial: &InitialEnvironment,
    experiment: Option<&pplx_space_node_runtime_core::catalog::BenchmarkEnvironment>,
    admit_new: bool,
    reference: Option<pplx_space_common_core::types::EnvironmentReference>,
    mode: ExecutionSelectionMode,
) -> Result<pplx_space_execution_common::environment::EnvironmentDefinition, Status> {
    let initial = initial
        .definition()
        .map_err(|error| Status::failed_precondition(describe(&error)))?;
    if reference.is_none_or(|reference| reference == initial.reference()) {
        return Ok(initial);
    }
    if let Some(experiment) = experiment {
        let definition = experiment
            .definition()
            .map_err(|error| Status::failed_precondition(describe(&error)))?;
        if reference == Some(definition.reference()) {
            if mode == ExecutionSelectionMode::PersistedStateRestore {
                return Err(Status::failed_precondition(
                    "experimental environments support cold boot only",
                ));
            }
            if mode == ExecutionSelectionMode::NewColdBoot && !admit_new {
                return Err(Status::failed_precondition(
                    "new experimental execution is disabled on this node",
                ));
            }
            return Ok(definition);
        }
    }
    Err(Status::failed_precondition(
        "unknown compiled execution environment",
    ))
}

const ALLOW_ROOT_OR_RTMGR: &[UdsPeerPrincipal] = &[
    UdsPeerPrincipal::Root,
    UdsPeerPrincipal::Service(user_names::SPACE_RTMGR),
];

/// The largest staged file a publication copies: well above any release
/// binary or guest kernel.
const MAX_ARTIFACT_FILE_BYTES: u64 = 512 * 1024 * 1024;

/// An artifact a request names.
#[derive(Clone, Debug)]
enum Artifact {
    Release(RuntimeIdentity, pplx_space_common_core::types::Digest),
    Kernel(ArtifactVersion, pplx_space_common_core::types::Digest),
}

impl Artifact {
    fn belongs_to_initial(&self, initial: &InitialEnvironment) -> bool {
        match self {
            Self::Release(release, digest) => {
                *release == initial.release().release && *digest == initial.release().digest()
            }
            Self::Kernel(_, digest) => *digest == initial.kernel().sha256,
        }
    }

    fn belongs_to_benchmark(&self) -> Result<bool, Status> {
        Ok(
            benchmark_environment()?.is_some_and(|experiment| match self {
                Self::Release(release, digest) => {
                    release == &experiment.release().release
                        && *digest == experiment.release().digest()
                }
                Self::Kernel(_, digest) => *digest == experiment.kernel().sha256,
            }),
        )
    }

    fn check_removal(
        &self,
        initial: &InitialEnvironment,
        policy: crate::config::ExperimentPolicy,
    ) -> Result<(), Status> {
        // E0 remains protected even without discovered inventory or after restart.
        if self.belongs_to_initial(initial) || (policy.retains() && self.belongs_to_benchmark()?) {
            Err(Status::failed_precondition(
                "artifact belongs to compiled E0 or a retained experimental environment",
            ))
        } else {
            Ok(())
        }
    }
    fn parse(artifact: Option<&NodeArtifact>) -> Result<Self, Status> {
        match artifact.and_then(|artifact| artifact.artifact.as_ref()) {
            Some(node_artifact::Artifact::RuntimeRelease(release)) => {
                let named =
                    pplx_space_execution_common::proto::RuntimeKind::try_from(release.runtime_kind)
                        .map_err(|_| Status::invalid_argument("unknown runtime kind"))?;
                let kind = pplx_space_common_core::types::RuntimeKind::try_from(named)
                    .map_err(|error| Status::invalid_argument(error.to_string()))?;
                let version = RuntimeVersion::parse(&release.version)
                    .map_err(|error| Status::invalid_argument(error.to_string()))?;
                let digest = release
                    .digest
                    .parse()
                    .map_err(|_| Status::invalid_argument("invalid runtime digest"))?;
                Ok(Self::Release(RuntimeIdentity { kind, version }, digest))
            }
            Some(node_artifact::Artifact::Component(component)) => {
                use pplx_space_execution_common::proto::ArtifactComponentKind;
                match ArtifactComponentKind::try_from(component.kind) {
                    Ok(ArtifactComponentKind::Kernel) => {}
                    Ok(ArtifactComponentKind::Unspecified) | Err(_) => {
                        return Err(Status::invalid_argument("a component kind is required"));
                    }
                    Ok(_) => {
                        return Err(Status::failed_precondition(
                            "no publisher for this component kind",
                        ));
                    }
                }
                let version = ArtifactVersion::parse(&component.version)
                    .map_err(|error| Status::invalid_argument(error.to_string()))?;
                let digest = component
                    .digest
                    .parse()
                    .map_err(|_| Status::invalid_argument("invalid component digest"))?;
                Ok(Self::Kernel(version, digest))
            }
            None => Err(Status::invalid_argument("name the artifact")),
        }
    }
}

pub(super) fn execution_inventory(
    paths: &crate::config::RuntimePaths,
    policy: crate::config::ExperimentPolicy,
) -> Result<super::RuntimeInventory, Status> {
    if execution_roots_overlap(paths)
        .map_err(|error| Status::failed_precondition(describe(&error)))?
    {
        tracing::warn!(
            "managed provisioning disabled; retaining prototype external execution until layout migration"
        );
        return super::RuntimeInventory::discover(
            [
                pplx_space_node_runtime_core::runtime_inventory::RuntimeInstallationRoot::new(
                    pplx_space_common_core::types::RuntimeKind::Firecracker,
                    &paths.vendor,
                ),
            ],
            &paths.assets,
            &paths.kernels,
        )
        .map_err(super::RuntimeInventoryError::into_status);
    }
    let initial = initial_environment()?;
    let store = ArtifactStore::new(
        &paths.store.releases,
        &paths.store.kernels,
        &paths.store.staging,
        MAX_ARTIFACT_FILE_BYTES,
    );
    let mut inventory = super::RuntimeInventory::discover_with_store(
        &store,
        initial,
        &paths.vendor,
        &paths.assets,
        &paths.kernels,
    )
    .map_err(super::RuntimeInventoryError::into_status)?;
    if policy.retains()
        && let Some(experiment) = benchmark_environment()?
    {
        inventory
            .include_benchmark(&store, experiment, &paths.kernels)
            .map_err(super::RuntimeInventoryError::into_status)?;
    }
    Ok(inventory)
}

pub(super) fn benchmark_environment()
-> Result<Option<&'static pplx_space_node_runtime_core::catalog::BenchmarkEnvironment>, Status> {
    static BENCHMARK: std::sync::OnceLock<
        Result<Option<pplx_space_node_runtime_core::catalog::BenchmarkEnvironment>, Status>,
    > = std::sync::OnceLock::new();
    BENCHMARK
        .get_or_init(|| {
            pplx_space_node_runtime_core::catalog::BenchmarkEnvironment::for_architecture(
                host_arch()?,
            )
            .map_err(|error| Status::failed_precondition(describe(&error)))
        })
        .as_ref()
        .map(Option::as_ref)
        .map_err(Clone::clone)
}

pub(super) fn initial_environment() -> Result<&'static InitialEnvironment, Status> {
    static INITIAL: std::sync::OnceLock<Result<InitialEnvironment, Status>> =
        std::sync::OnceLock::new();
    INITIAL
        .get_or_init(|| {
            InitialEnvironment::for_architecture(host_arch()?)
                .map_err(|error| Status::failed_precondition(describe(&error)))
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn host_arch() -> Result<Arch, Status> {
    std::env::consts::ARCH.parse().map_err(|_| {
        Status::failed_precondition(format!(
            "node architecture {} has no artifact pins",
            std::env::consts::ARCH
        ))
    })
}

/// `error` and every error that caused it, as one line.
fn describe(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(next) = cause {
        text.push_str(": ");
        text.push_str(&next.to_string());
        cause = next.source();
    }
    text
}

fn publish_status(error: PublishError) -> Status {
    match error {
        PublishError::InvalidVersion { .. }
        | PublishError::Mismatch { .. }
        | PublishError::WrongSize { .. }
        | PublishError::NotRegularFile { .. }
        | PublishError::Linked { .. }
        | PublishError::TooLarge { .. } => Status::invalid_argument(describe(&error)),
        PublishError::ExistingArtifact(ref source) if source.is_transient() => {
            Status::internal(describe(&error))
        }
        PublishError::UnsafeRoot { .. } | PublishError::ExistingArtifact(_) => {
            Status::failed_precondition(describe(&error))
        }
        PublishError::Source { .. } | PublishError::Io { .. } => Status::internal(describe(&error)),
    }
}

fn remove_status(error: RemoveError) -> Status {
    match error {
        RemoveError::InvalidVersion { .. } => Status::invalid_argument(describe(&error)),
        RemoveError::IdentityChanged { .. } | RemoveError::UnsafeRoot { .. } => {
            Status::failed_precondition(describe(&error))
        }
        RemoveError::Io { .. } => Status::internal(describe(&error)),
    }
}

/// Earlier prototype images linked execution roots into the store. Refuse
/// mutations there while retaining the existing external execution path. Root-owned deployment
/// configuration is stable during a mutation; recheck it on each request.
fn separate_store(paths: &crate::config::RuntimePaths) -> Result<(), std::io::Error> {
    if execution_roots_overlap(paths)? {
        Err(std::io::Error::other(
            "execution and managed artifact roots overlap; repair provisioning before managing artifacts",
        ))
    } else {
        Ok(())
    }
}

fn execution_roots_overlap(paths: &crate::config::RuntimePaths) -> Result<bool, std::io::Error> {
    fn resolve(path: &std::path::Path) -> Result<std::path::PathBuf, std::io::Error> {
        match std::fs::canonicalize(path) {
            Ok(path) => Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // An absent root is fine, but a dangling execution link is not
                // evidence that it is separate from the managed store.
                match std::fs::symlink_metadata(path) {
                    Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => {
                        Ok(path.to_path_buf())
                    }
                    _ => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }
    let vendor_kernels = std::path::Path::new(
        pplx_space_common_core::host::linux::paths::FIRECRACKER_KERNEL_VENDOR_DIR,
    );
    for managed in [&paths.store.releases, &paths.store.kernels] {
        let managed = resolve(managed)?;
        for execution in [
            paths.vendor.as_path(),
            &paths.assets,
            &paths.kernels,
            vendor_kernels,
        ] {
            let execution = resolve(execution)?;
            if execution.starts_with(&managed) || managed.starts_with(&execution) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

impl NodeManagerService {
    /// The slot an artifact-store change runs in, or `RESOURCE_EXHAUSTED`
    /// while another holds it, which the runtime manager retries.
    fn artifact_slot(&self) -> Result<tokio::sync::OwnedSemaphorePermit, Status> {
        self.artifact_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("the artifact store is busy; retry"))
    }

    fn artifact_store(&self) -> ArtifactStore {
        let paths = &self.runtime_paths.store;
        ArtifactStore::new(
            &paths.releases,
            &paths.kernels,
            &paths.staging,
            MAX_ARTIFACT_FILE_BYTES,
        )
    }

    /// Best-effort preparation of the separate managed store. Existing launch
    /// roots and aliases are never moved or repaired here.
    pub(crate) async fn prepare_artifact_store(&self) {
        let store = self.artifact_store();
        let paths = self.runtime_paths.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            separate_store(&paths)?;
            store.prepare_roots()?;
            if let Some(account) = nix::unistd::User::from_name(user_names::SPACE_RTMGR)
                .map_err(std::io::Error::other)?
            {
                store.ensure_staging_root(account.uid.as_raw(), account.gid.as_raw())?;
            }
            Ok::<_, std::io::Error>(())
        })
        .await;
        match prepared {
            Ok(Ok(())) => tracing::info!("managed artifact store prepared"),
            Ok(Err(error)) => {
                tracing::warn!(%error, "artifact store preparation incomplete; requests recheck the store; execution roots unchanged")
            }
            Err(error) => {
                tracing::warn!(%error, "artifact store preparation failed; execution roots unchanged")
            }
        }
        // Reclaim independently of staging/account provisioning, and never hold
        // node readiness behind recursive deletion. Tracked for shutdown.
        let store = self.artifact_store();
        let paths = self.runtime_paths.clone();
        self.device_op_tasks.spawn_blocking(move || {
            let result = separate_store(&paths).and_then(|()| store.clear_leftovers());
            match result {
                Ok(cleared) => tracing::info!(cleared, "managed artifact leftovers cleared"),
                Err(error) => {
                    tracing::warn!(%error, "managed artifact cleanup failed; retry on next startup")
                }
            }
        });
    }

    pub(super) async fn publish_artifact(
        &self,
        request: Request<PublishArtifactRequest>,
    ) -> Result<Response<PublishArtifactResponse>, Status> {
        self.authorize_peer(&request, "PublishArtifact", ALLOW_ROOT_OR_RTMGR)?;
        let request = request.into_inner();
        let artifact = Artifact::parse(request.artifact.as_ref())?;
        let staged = StagedName::parse(request.staging_name)
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let architecture = host_arch()?;
        let store = self.artifact_store();
        let paths = self.runtime_paths.clone();
        let catalog_dir = self.runtime_paths.store.catalog.clone();
        let publication = self.publish_artifact_work(artifact.clone(), move || {
                separate_store(&paths)
                    .map_err(|error| Status::failed_precondition(describe(&error)))?;
                let catalog = RuntimeCatalog::load(&catalog_dir)
                    .map_err(|error| Status::failed_precondition(describe(&error)))?;
                match &artifact {
                    Artifact::Release(release, digest) => {
                        let pin = catalog
                            .release(release, architecture)
                            .ok_or_else(|| {
                                Status::failed_precondition(format!(
                                    "{release} has no pins for {architecture}"
                                ))
                            })?;
                        if pin.digest() != *digest {
                            return Err(Status::failed_precondition(format!("{release} requested digest {digest} differs from current catalog digest {}", pin.digest())));
                        }
                        store.publish_release(pin, &staged).map_err(publish_status)
                    }
                    Artifact::Kernel(version, digest) => {
                        let pin = catalog
                            .kernel(version, architecture)
                            .ok_or_else(|| {
                                Status::failed_precondition(format!(
                                    "kernel {version} has no pins for {architecture}"
                                ))
                            })?;
                        if pin.sha256 != *digest {
                            return Err(Status::failed_precondition(format!("kernel {version} requested digest {digest} differs from current catalog digest {}", pin.sha256)));
                        }
                        store.publish_kernel(pin, &staged).map_err(publish_status)
                    }
                }
            }).await?;
        Ok(Response::new(PublishArtifactResponse {
            path: publication.path.to_string_lossy().into_owned(),
            digest: publication.digest.to_string(),
            already_present: publication.outcome == PublishOutcome::AlreadyPresent,
        }))
    }

    async fn publish_artifact_work(
        &self,
        artifact: Artifact,
        publish: impl FnOnce() -> Result<Publication, Status> + Send + 'static,
    ) -> Result<Publication, Status> {
        let initial = initial_environment()?;
        let policy = self.experiment_policy;
        let affects_execution = artifact.belongs_to_initial(initial)
            || (policy.retains() && artifact.belongs_to_benchmark()?);
        let slot = self.artifact_slot()?;
        // Serialize selected publication with discovery, while readers keep the
        // previous immutable snapshot. Unselected artifacts skip this refresh lock, but
        // discovery can still wait for their store publication locks.
        let guard = if affects_execution {
            Some(
                self.runtime_inventory_refresh_lock
                    .clone()
                    .lock_owned()
                    .await,
            )
        } else {
            None
        };
        let snapshot = self.runtime_inventory.clone();
        let paths = self.runtime_paths.clone();
        self.off_runtime("publish artifact", move || {
            let _slot = slot;
            let _guard = guard;
            let publication = publish()?;
            let unchanged = publication.outcome == PublishOutcome::AlreadyPresent
                && snapshot.load_full().is_some_and(|inventory| inventory.check_files_unchanged());
            if affects_execution && !unchanged {
                // Build first, then swap. E0 verification failures withdraw only
                // that binding; a scan failure prevents advertising stale state.
                match execution_inventory(&paths, policy) {
                    Ok(inventory) => snapshot.store(Some(std::sync::Arc::new(inventory))),
                    Err(error) => {
                        snapshot.store(None);
                        tracing::warn!(%error, "artifact published but execution discovery is unavailable");
                    }
                }
            }
            Ok(publication)
        }).await
    }

    pub(super) async fn remove_artifact(
        &self,
        request: Request<RemoveArtifactRequest>,
    ) -> Result<Response<RemoveArtifactResponse>, Status> {
        self.authorize_peer(&request, "RemoveArtifact", ALLOW_ROOT_OR_RTMGR)?;
        let artifact = Artifact::parse(request.get_ref().artifact.as_ref())?;
        artifact.check_removal(initial_environment()?, self.experiment_policy)?;
        let store = self.artifact_store();
        let paths = self.runtime_paths.clone();
        let slot = self.artifact_slot()?;
        // Only unselected candidates reach removal. Neither external files
        // nor E0 can be removed, so executable inventory is unaffected.
        let outcome = self
            .off_runtime("remove artifact", move || {
                let _slot = slot;
                separate_store(&paths)
                    .map_err(|error| Status::failed_precondition(describe(&error)))?;
                match &artifact {
                    Artifact::Release(release, digest) => store
                        .remove_release(release, *digest)
                        .map_err(remove_status),
                    Artifact::Kernel(version, digest) => {
                        store.remove_kernel(version, *digest).map_err(remove_status)
                    }
                }
            })
            .await?;
        Ok(Response::new(RemoveArtifactResponse {
            removed: outcome == RemoveOutcome::Removed,
        }))
    }
}

#[cfg(test)]
mod tests;
