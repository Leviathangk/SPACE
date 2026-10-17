//! One read-only page per reconciliation tick. No candidate is a cleanup job.
//!
//! A walk spans several ticks, so its pages are summed and one summary is
//! logged when it ends. A walk is a live read, not a snapshot: a sandbox
//! created or torn down while it runs can show up once. A candidate the
//! previous complete walk also found is not that race, so the summary warns
//! when such a persistent candidate first appears, and when a walk could not
//! read every source. Candidates only one walk found, and persistent ones
//! already warned about, are logged at INFO.

use std::collections::HashSet;

use pplx_space_node_mgr_client::{
    NodeManagerGrpcClient,
    proto::{ListOrphanCandidatesRequest, ListOrphanCandidatesResponse},
};
use std::time::Duration;

/// Candidate names remembered per walk; any past this are only counted.
const MAX_TRACKED: usize = 4096;
const MAX_SAMPLES: usize = 10;
const MAX_WARNINGS: usize = 8;

#[derive(Default)]
pub(super) struct Observer {
    client: Option<NodeManagerGrpcClient>,
    cursor: String,
    walk: Walk,
    /// Every candidate the last complete walk found.
    previous: HashSet<String>,
    /// The persistent candidates the last complete walk reported.
    reported: HashSet<String>,
}

#[derive(Default)]
struct Walk {
    candidates: HashSet<String>,
    total: usize,
    untracked: usize,
    warnings: Vec<String>,
}

/// What a finished walk found, as one summary.
#[derive(Debug)]
struct WalkSummary {
    verdict: Verdict,
    candidates: usize,
    persistent: usize,
    new_persistent: usize,
    samples: Vec<String>,
    warnings: Vec<String>,
    complete: bool,
    scan_started_at_unix_ms: i64,
    observed_at_unix_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// No candidates, and every source was read.
    Clean,
    /// Candidates, none newly persistent: each is new this walk or was
    /// reported by an earlier one.
    Known,
    /// A candidate the previous complete walk also found, reported now for
    /// the first time.
    NewlyPersistent,
    /// A source could not be read, or more candidates than are tracked: the
    /// walk does not account for the whole node.
    Incomplete,
}

impl Observer {
    pub(super) async fn observe(&mut self) {
        if self.cursor.is_empty() {
            self.walk = Walk::default();
        }
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            if self.client.is_none() {
                self.client = Some(
                    pplx_space_node_mgr_client::connect()
                        .await
                        .map_err(|error| tonic::Status::unavailable(error.to_string()))?,
                );
            }
            self.client
                .as_mut()
                .expect("client connected above")
                .list_orphan_candidates(ListOrphanCandidatesRequest {
                    page_token: self.cursor.clone(),
                })
                .await
                .map(tonic::Response::into_inner)
        })
        .await;
        match result {
            Ok(Ok(page)) => {
                tracing::debug!(
                    complete = page.complete,
                    last_page = page.next_page_token.is_empty(),
                    observed_at_unix_ms = page.observed_at_unix_ms,
                    "read-only orphan inventory page observed"
                );
                self.cursor = page.next_page_token.clone();
                if let Some(summary) = self.record_page(page) {
                    summary.log();
                }
            }
            Ok(Err(status)) if status.code() == tonic::Code::ResourceExhausted => {
                // Admission/registry contention rejects before advancing a
                // page. Keep the cursor and retry on the next existing tick.
                tracing::debug!(%status, "orphan inventory busy; retaining cursor for retry");
            }
            Ok(Err(status)) if status.code() == tonic::Code::Unimplemented => {
                self.cursor.clear();
                tracing::debug!(
                    "node manager does not implement orphan inventory (local simulator); retrying next tick"
                );
            }
            error => {
                // A lost response may have consumed the cursor. Start another
                // walk next tick; the server bounds and expires abandoned walks.
                self.cursor.clear();
                self.client = None;
                tracing::warn!(
                    ?error,
                    "orphan inventory unavailable; retrying next node reconciliation tick"
                );
            }
        }
    }

    /// Adds a page to the current walk, and summarizes the walk if the page
    /// is its last.
    fn record_page(&mut self, page: ListOrphanCandidatesResponse) -> Option<WalkSummary> {
        let last_page = page.next_page_token.is_empty();
        let walk = &mut self.walk;
        for candidate in page
            .orphaned_namespaces
            .into_iter()
            .chain(page.orphaned_cgroups)
            .chain(page.orphaned_run_dirs)
            .chain(page.orphaned_cache_dirs)
            .chain(page.orphaned_nft_rules)
            .chain(page.orphaned_processes)
        {
            walk.total += 1;
            if walk.candidates.len() < MAX_TRACKED {
                walk.candidates.insert(candidate);
            } else if !walk.candidates.contains(&candidate) {
                walk.untracked += 1;
            }
        }
        let room = MAX_WARNINGS.saturating_sub(walk.warnings.len());
        walk.warnings.extend(page.warnings.into_iter().take(room));
        if !last_page {
            return None;
        }

        let walk = std::mem::take(&mut self.walk);
        let complete = page.complete && walk.untracked == 0;
        let mut persistent: Vec<_> = walk
            .candidates
            .intersection(&self.previous)
            .cloned()
            .collect();
        persistent.sort();
        let mut new_persistent: Vec<_> = persistent
            .iter()
            .filter(|candidate| !self.reported.contains(*candidate))
            .cloned()
            .collect();
        let verdict = if !complete {
            Verdict::Incomplete
        } else if !new_persistent.is_empty() {
            Verdict::NewlyPersistent
        } else if walk.total > 0 {
            Verdict::Known
        } else {
            Verdict::Clean
        };
        let new_persistent_count = new_persistent.len();
        // Newly persistent candidates are the ones to inspect, so they lead.
        let mut rest: Vec<_> = walk
            .candidates
            .iter()
            .filter(|candidate| !new_persistent.contains(*candidate))
            .cloned()
            .collect();
        rest.sort();
        new_persistent.extend(rest);
        new_persistent.truncate(MAX_SAMPLES);
        // An incomplete walk can miss a candidate that is still there, so it
        // neither starts nor ends persistence.
        if complete {
            self.previous = walk.candidates;
            self.reported = persistent.iter().cloned().collect();
        }
        Some(WalkSummary {
            verdict,
            candidates: walk.total,
            persistent: persistent.len(),
            new_persistent: new_persistent_count,
            samples: new_persistent,
            warnings: walk.warnings,
            complete,
            scan_started_at_unix_ms: page.scan_started_at_unix_ms,
            observed_at_unix_ms: page.observed_at_unix_ms,
        })
    }
}

impl WalkSummary {
    fn log(&self) {
        macro_rules! summary {
            ($level:ident) => {
                tracing::$level!(
                    candidates = self.candidates,
                    persistent = self.persistent,
                    new_persistent = self.new_persistent,
                    samples = ?self.samples,
                    warnings = ?self.warnings,
                    complete = self.complete,
                    scan_started_at_unix_ms = self.scan_started_at_unix_ms,
                    observed_at_unix_ms = self.observed_at_unix_ms,
                    "read-only orphan inventory; inspect candidates before any manual cleanup"
                )
            };
        }
        match self.verdict {
            Verdict::Incomplete | Verdict::NewlyPersistent => summary!(warn),
            Verdict::Known => summary!(info),
            Verdict::Clean => summary!(debug),
        }
    }
}

#[cfg(test)]
mod tests;
