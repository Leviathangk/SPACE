//! Public operation lifecycle counters.
//!
//! - `operations.requeued{api_product, kind, reason}` counts every re-queue.
//!   `reason:teardown_redrive` is a delete re-driving a teardown it could not
//!   confirm, a requeue that spends no retry budget and has no deadline.
//! - `operations.delete_redrive{api_product, cause, redrive_age}` counts each
//!   of those re-drives with what a monitor needs to judge it. `cause` is
//!   `teardown_unconfirmed`, or the catch-all `row_left_deleting`, where any
//!   other error left the row claimed `deleting` and a permanent fault would
//!   re-drive forever. `redrive_age` is a lower bound on how long that delete
//!   has been re-driving: `under_5m`, `5m_to_30m`, `30m_to_2h` or `over_2h`.
//!   "A delete has been re-driving for more than 30 minutes" is any
//!   `redrive_age:30m_to_2h` or `redrive_age:over_2h` count. A stuck delete
//!   re-drives about every 5 minutes, so that count stays non-zero for as
//!   long as one is stuck.

use std::time::Duration;

use crate::emit::{counter, tag};

pub fn created(api_product: &str, kind: &str, target_type: &str) {
    counter(
        "operations.created",
        1,
        &[
            tag("api_product", api_product),
            tag("kind", kind),
            tag("target_type", target_type),
        ],
    );
}

pub fn claimed(api_product: &str, kind: &str) {
    counter(
        "operations.claimed",
        1,
        &[tag("api_product", api_product), tag("kind", kind)],
    );
}

pub fn completed(api_product: &str, kind: &str, state: &str) {
    counter(
        "operations.completed",
        1,
        &[
            tag("api_product", api_product),
            tag("kind", kind),
            tag("state", state),
        ],
    );
}

pub fn failed(api_product: &str, kind: &str, error_code: &str) {
    counter(
        "operations.failed",
        1,
        &[
            tag("api_product", api_product),
            tag("kind", kind),
            tag("error_code", error_code),
        ],
    );
}

pub fn requeued(api_product: &str, kind: &str, reason: &str) {
    counter(
        "operations.requeued",
        1,
        &[
            tag("api_product", api_product),
            tag("kind", kind),
            tag("reason", reason),
        ],
    );
}

pub fn deadline_exceeded(api_product: &str, kind: &str, last_requeue_reason: &str) {
    counter(
        "operations.deadline_exceeded",
        1,
        &[
            tag("api_product", api_product),
            tag("kind", kind),
            tag("last_requeue_reason", last_requeue_reason),
        ],
    );
}

pub fn stale_claim(api_product: &str, kind: &str) {
    counter(
        "operations.stale_claim",
        1,
        &[tag("api_product", api_product), tag("kind", kind)],
    );
}

/// One re-drive of a delete that keeps its sandbox row claimed `deleting`.
/// `redriving_for_at_least` is the caller's lower bound on how long the
/// delete has been re-driving; it is bucketed into `redrive_age`.
pub fn delete_redrive(api_product: &str, cause: &str, redriving_for_at_least: Duration) {
    let redrive_age = match redriving_for_at_least.as_secs() {
        0..300 => "under_5m",
        300..1_800 => "5m_to_30m",
        1_800..7_200 => "30m_to_2h",
        _ => "over_2h",
    };
    counter(
        "operations.delete_redrive",
        1,
        &[
            tag("api_product", api_product),
            tag("cause", cause),
            tag("redrive_age", redrive_age),
        ],
    );
}
