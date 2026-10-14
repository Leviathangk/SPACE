//! Native Hyper tasks belong to the HTTP connection that created them.
use std::future::Future;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing::Instrument;

#[derive(Clone)]
pub(super) struct Executor {
    pub(super) tasks: TaskTracker,
    shutdown: CancellationToken,
}
impl Executor {
    pub(super) fn new(shutdown: CancellationToken) -> Self {
        Self {
            tasks: TaskTracker::new(),
            shutdown,
        }
    }
    pub(super) async fn shutdown(&self) {
        self.shutdown.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
}
impl<F> hyper::rt::Executor<F> for Executor
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, future: F) {
        let shutdown = self.shutdown.clone();
        self.tasks.spawn(
            async move {
                tokio::select! { biased; () = shutdown.cancelled() => {}, _ = future => {} }
            }
            .in_current_span(),
        );
    }
}
