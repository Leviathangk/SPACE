use super::*;

impl RuntimeInventory {
    /// Build an executable snapshot from external layouts and the sole managed
    /// binding. Other managed candidates are deliberately not scanned here.
    pub fn discover_with_store(
        store: &crate::artifact_store::ArtifactStore,
        initial: &crate::catalog::InitialEnvironment,
        vendor: &Path,
        assets: &Path,
        kernels: &Path,
    ) -> Result<Self, RuntimeInventoryError> {
        use crate::artifact_store::{
            managed_kernel_directory, managed_release_directory, release_directory,
        };
        let mut inventory = Self::discover(
            [RuntimeInstallationRoot::new(
                RuntimeKind::Firecracker,
                vendor,
            )],
            assets,
            kernels,
        )?;
        let managed_roots = [store.releases_root(), store.kernels_root()]
            .into_iter()
            .map(canonicalize_inventory_path)
            .collect::<Result<Vec<_>, _>>()?;
        let external = |path: &Path| -> Result<bool, RuntimeInventoryError> {
            Ok(canonicalize_inventory_path(path)?.is_some_and(|resolved| {
                !managed_roots
                    .iter()
                    .flatten()
                    .any(|root| resolved.starts_with(root))
            }))
        };
        // A root-owned legacy alias must not smuggle an unselected/removable
        // managed candidate into the external execution inventory.
        let mut runtimes = Vec::new();
        for runtime in inventory.runtimes {
            let mut is_external = true;
            for file in runtime.files.files.values() {
                is_external &= external(file)?;
            }
            if is_external {
                runtimes.push(runtime);
            }
        }
        inventory.runtimes = runtimes;
        let mut components = Vec::new();
        for component in inventory.components {
            if external(&component.path)? {
                components.push(component);
            }
        }
        inventory.components = components;
        let managed_release = store.releases_root().join(managed_release_directory(
            &initial.release.release,
            initial.release.digest(),
        ));
        let baked_release = vendor.join(release_directory(&initial.release.release));
        // Identical copies prefer the durable managed store. No alias changes
        // or adoption of the external copy are involved.
        inventory
            .runtimes
            .retain(|entry| entry.runtime != initial.release.release);
        if managed_release
            .try_exists()
            .map_err(|source| RuntimeInventoryError::ReadDir {
                path: managed_release.clone(),
                source,
            })?
        {
            store.verify_release(&initial.release)?;
            inventory.add_initial_release(initial, &managed_release)?;
        } else if baked_release
            .try_exists()
            .map_err(|source| RuntimeInventoryError::ReadDir {
                path: baked_release.clone(),
                source,
            })?
        {
            store.verify_release_at(&initial.release, &baked_release)?;
            inventory.add_initial_release(initial, &baked_release)?;
        }
        let managed_kernel = store.kernels_root().join(managed_kernel_directory(
            &initial.kernel.version,
            initial.kernel.sha256,
        ));
        if managed_kernel
            .try_exists()
            .map_err(|source| RuntimeInventoryError::ReadDir {
                path: managed_kernel.clone(),
                source,
            })?
        {
            let parent = kernels
                .parent()
                .ok_or_else(|| RuntimeInventoryError::Empty(kernels.to_path_buf()))?;
            let cache = store.prepare_execution_cache(parent)?;
            let staged = store.stage_kernel(&initial.kernel, &cache)?;
            inventory.add_initial_kernel(
                initial,
                staged.path.join(crate::artifact_store::KERNEL_IMAGE),
            )?;
        } else {
            for component in &inventory.components {
                if component.metadata.digest == Some(initial.kernel.sha256) {
                    store.verify_kernel_at(&initial.kernel, &component.path)?;
                }
            }
        }
        inventory.bind_initial_environment(initial);
        inventory.fingerprints = inventory
            .paths()
            .map(|path| {
                std::fs::metadata(path)
                    .map(|md| (path.to_path_buf(), FileFingerprint::of(&md)))
                    .map_err(|source| RuntimeInventoryError::ProbeInventoryPath {
                        path: path.to_path_buf(),
                        source,
                    })
            })
            .collect::<Result<_, _>>()?;
        Ok(inventory)
    }

    /// Bind new and unannotated legacy boots to approved E0 bytes. Explicit
    /// historical component identities remain available independently.
    fn bind_initial_environment(&mut self, initial: &crate::catalog::InitialEnvironment) {
        self.defaults = vec![initial.release.release.clone()];
        for component in &mut self.components {
            let is_initial = component.metadata.kind == ArtifactComponentKind::Kernel
                && component.metadata.digest == Some(initial.kernel.sha256);
            component.default_for_new = is_initial;
            component.legacy_default = is_initial;
        }
    }

    /// Only the compiled binding enters execution from the managed store.
    /// Publishing another approved candidate does not make it executable.
    fn add_initial_release(
        &mut self,
        initial: &crate::catalog::InitialEnvironment,
        root: &Path,
    ) -> Result<(), RuntimeInventoryError> {
        let installation = FirecrackerDriver
            .inspect_installation(root.to_path_buf(), initial.release.release.version.clone())?;
        self.runtimes
            .retain(|entry| entry.runtime != installation.runtime);
        self.runtimes.push(installation);
        Ok(())
    }

    fn add_initial_kernel(
        &mut self,
        initial: &crate::catalog::InitialEnvironment,
        path: PathBuf,
    ) -> Result<(), RuntimeInventoryError> {
        self.components
            .retain(|entry| entry.metadata.digest != Some(initial.kernel.sha256));
        self.components.push(kernel_installation(
            kernel_metadata(initial.kernel.version.clone(), initial.kernel.sha256)?,
            path,
            true,
            true,
        ));
        Ok(())
    }
}
