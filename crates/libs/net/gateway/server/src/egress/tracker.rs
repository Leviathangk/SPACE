//! Registry of established TCP flows, reconciled on policy changes.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use super::evidence::TcpAdmissionEvidence;

/// Established-flow registry shared by admission and policy resweeps.
#[derive(Clone, Debug, Default)]
pub(crate) struct DnsAttributedTcpTracker {
    inner: Arc<DnsAttributedInner>,
}

#[derive(Debug, Default)]
struct DnsAttributedInner {
    next_id: AtomicU64,
    connections: Mutex<HashMap<u64, DnsAttributedConnection>>,
}

#[derive(Debug)]
struct DnsAttributedConnection {
    admission: TcpAdmissionEvidence,
    shutdown_token: CancellationToken,
}

pub(crate) struct TcpConnectionGuard {
    id: u64,
    tracker: DnsAttributedTcpTracker,
}

impl DnsAttributedTcpTracker {
    /// Register an established flow. The guard unregisters on drop, so the
    /// caller must hold it for the lifetime of the connection.
    #[must_use]
    pub(crate) fn register(
        &self,
        admission: TcpAdmissionEvidence,
        shutdown_token: CancellationToken,
    ) -> TcpConnectionGuard {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner.connections.lock().insert(
            id,
            DnsAttributedConnection {
                admission,
                shutdown_token,
            },
        );
        TcpConnectionGuard {
            id,
            tracker: self.clone(),
        }
    }

    /// Cancel every tracked flow when there is no policy left to authorize it.
    pub fn cancel_all(&self) -> usize {
        self.cancel_where(|_| true)
    }

    /// Cancel matching registrations, without interpreting their evidence.
    pub(crate) fn cancel_where(&self, predicate: impl Fn(&TcpAdmissionEvidence) -> bool) -> usize {
        let tokens = self
            .inner
            .connections
            .lock()
            .values()
            .filter(|connection| {
                !connection.shutdown_token.is_cancelled() && predicate(&connection.admission)
            })
            .map(|connection| connection.shutdown_token.clone())
            .collect::<Vec<_>>();
        let cancelled = tokens.len();
        tokens.into_iter().for_each(|token| token.cancel());
        cancelled
    }

    fn unregister(&self, id: u64) {
        self.inner.connections.lock().remove(&id);
    }
}

impl Drop for TcpConnectionGuard {
    fn drop(&mut self) {
        self.tracker.unregister(self.id);
    }
}

#[cfg(test)]
mod tests;
