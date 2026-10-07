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

/// One attempt to undo what a failed operation left behind before its failure
/// is recorded. `outcome` is the executor's verdict (for a sandbox start:
/// `retired`, `nothing_to_retire`, `running_pod_left`, a `deferred_*` reason,
/// or an error class), or `abandoned` when the dispatcher records the failure
/// because compensation did not complete within its bound.
pub fn compensated(api_product: &str, kind: &str, outcome: &str) {
    counter(
        "operations.compensated",
        1,
        &[
            tag("api_product", api_product),
            tag("kind", kind),
            tag("outcome", outcome),
        ],
    );
}
