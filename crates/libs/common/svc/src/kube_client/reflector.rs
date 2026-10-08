//! Shared driver for watcher-backed Kubernetes stores.

use kube::{
    Api,
    runtime::{reflector, watcher},
};
use tokio::sync::watch;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

/// Current synchronization health of a reflector's store.
#[derive(Clone, Debug)]
pub struct ReflectorHealth {
    synced: watch::Receiver<bool>,
}

impl ReflectorHealth {
    /// A full list has completed with no subsequent observed watch failure.
    /// Returns false during relisting and after the task stops, including panic.
    /// Ordinary watch propagation delay still applies.
    pub fn is_synced(&self) -> bool {
        *self.synced.borrow() && self.synced.has_changed().is_ok()
    }
}

/// Drive a reflector until shutdown, relisting after errors with the default
/// watcher backoff. Callers choose whether to require current synchronization
/// via the returned health handle or only initial readiness via their store.
pub fn spawn_reflector<K>(
    label: &'static str,
    mut writer: reflector::store::Writer<K>,
    api: Api<K>,
    config: watcher::Config,
    shutdown: CancellationToken,
) -> ReflectorHealth
where
    K: kube::Resource
        + Clone
        + serde::de::DeserializeOwned
        + std::fmt::Debug
        + Send
        + Sync
        + 'static,
    K::DynamicType: Default + Eq + std::hash::Hash + Clone,
{
    let (synced, health) = watch::channel(false);
    tokio::spawn(async move {
        let mut backoff = watcher::DefaultBackoff::default();
        loop {
            let mut stream = std::pin::pin!(watcher::watcher(api.clone(), config.clone()));
            loop {
                let event = tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => return,
                    event = stream.next() => event,
                };
                match event {
                    Some(Ok(event)) => {
                        if matches!(event, watcher::Event::Init) {
                            synced.send_replace(false);
                        }
                        writer.apply_watcher_event(&event);
                        if matches!(event, watcher::Event::InitDone) {
                            synced.send_replace(true);
                        }
                    }
                    Some(Err(err)) => {
                        synced.send_replace(false);
                        tracing::warn!(reflector = label, error = %err, "watcher error; relisting after backoff");
                        break;
                    }
                    None => {
                        tracing::warn!(reflector = label, "watcher stream ended");
                        return;
                    }
                }
            }
            // Recreate the watcher so recovery ends at a complete list even
            // when a resumed watch would have no object updates to emit.
            let Some(delay) = backoff.next() else { return };
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(delay) => {}
            }
        }
    });
    ReflectorHealth { synced: health }
}

#[cfg(test)]
mod tests;
