//! Build-local ownership of the expected declaration. No artifact projection or
//! host filesystem parsing: all inspection and writes happen in the build VM.
use crate::{
    BuildError,
    recipe::{Origin, Recipe, Step},
    ssh::{SshError, SshSession},
};
use pplx_space_common_core::types::MAX_TEMPLATE_RUNTIME_BYTES;

const INSPECT: &str = "/usr/local/bin/spaced-svc --runtime-inspect";
const DETECT: &str = "if test -e /etc/systemd/system/space-runtime.service || test -L /etc/systemd/system/space-runtime.service || test -e /etc/space/runtime.json || test -L /etc/space/runtime.json; then printf native; fi";
const CLEAR: &str = "rm -f -- /etc/space/runtime.json";
const WRITE: &str = "set -eu; test ! -L /etc/space; mkdir -p /etc/space; rm -f /etc/space/.runtime-build; umask 077; cat > /etc/space/.runtime-build; mv -fT /etc/space/.runtime-build /etc/space/runtime.json";

pub(crate) struct RuntimeBuild {
    native: bool,
    expected: Vec<u8>,
}
impl RuntimeBuild {
    pub async fn prepare(ssh: &SshSession, recipe: &Recipe) -> Result<Self, BuildError> {
        let edit = recipe.steps.last().map(|step| &step.step);
        let explicit = matches!(edit, Some(Step::SetRuntime(_) | Step::ClearRuntime));
        let native = explicit || !ssh.exchange(DETECT, &[], 16).await?.is_empty();
        if !native {
            return Ok(Self {
                native: false,
                expected: Vec::new(),
            });
        }
        // OCI payloads and the guest bundle must not import startup intent.
        // This happens before any tenant recipe step, including SetRuntime.
        if matches!(recipe.origin, Origin::FromOciImage { .. }) {
            ssh.exchange(CLEAR, &[], 0).await?;
        }
        let parent = ssh
            .exchange(INSPECT, &[], MAX_TEMPLATE_RUNTIME_BYTES)
            .await?;
        let expected = match edit {
            Some(Step::SetRuntime(runtime)) => runtime
                .canonical_bytes()
                .map_err(|error| BuildError::message(error.to_string()))?,
            Some(Step::ClearRuntime) => Vec::new(),
            _ => parent,
        };
        Ok(Self { native, expected })
    }

    pub async fn apply_step(&self, ssh: &SshSession, step: &Step) -> Result<(), SshError> {
        match step {
            Step::SetRuntime(_) => {
                ssh.exchange(WRITE, &self.expected, 0).await?;
            }
            Step::ClearRuntime => {
                ssh.exchange(CLEAR, &[], 0).await?;
            }
            _ => {}
        }
        Ok(())
    }

    pub async fn finalize(&self, ssh: &SshSession) -> Result<(), BuildError> {
        if !self.native {
            if !ssh.exchange(DETECT, &[], 16).await?.is_empty() {
                return Err(BuildError::message(
                    "runtime backend introduced by a recipe step; rebuild from a capable base and use SetRuntime",
                ));
            }
            return Ok(());
        }
        let actual = ssh
            .exchange(INSPECT, &[], MAX_TEMPLATE_RUNTIME_BYTES)
            .await?;
        if actual != self.expected {
            return Err(BuildError::message(
                "runtime declaration changed outside its recipe operation; use SetRuntime or ClearRuntime",
            ));
        }
        Ok(())
    }

    pub fn requires_durable_stop(&self) -> bool {
        self.native
    }
}
