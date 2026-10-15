//! Metric helpers for BYOK per-sandbox volume encryption (dm-crypt).
//!
//! Names are emitted under `byok.*`.
//!
//! ## Why these exist
//!
//! Every BYOK failure is fail-closed and surfaces to the caller as
//! `Status::internal` with the underlying gRPC code flattened into the
//! message string, so an operator reading an API response cannot tell
//! "this node is missing dm_crypt" from "credmgr refused the key" from
//! "the disk is full". Those three need completely different responses.
//! The `phase` dimension below is that distinction, recorded where the
//! typed error still exists.
//!
//! ## Dimensions
//!
//! - `op` — which device stack: [`OP_ROOTFS`] (the dm-snapshot composite
//!   over an encrypted COW that Firecracker boots from),
//!   [`OP_SNAPSHOT_VOLUME`] (the encrypted filesystem a suspend's vmstate
//!   and memory are written into), [`OP_POOL_ROOTFS`] (the pre-boot reflink
//!   retained alongside an ordinary warm-pool rootfs).
//! - `phase` — where in the prepare/release the failure happened, derived
//!   from the primitive's error type rather than from a message string, so
//!   a new error variant cannot silently land in the wrong bucket.
//! - `reason` — for DEK fetches, whether credmgr was unreachable, refused,
//!   or failed some other way. Distinguishes an authority outage from a
//!   revoked or unenrolled org.
//!
//! `byok.pool_adoption.duration_ms` measures an owned sandbox-manager adoption,
//! including lock wait and replay, with `encrypted` and `outcome` (success or
//! failure) tags. Failed attempts are included. Phase spans break this down
//! into pause, DEK lookup, drive switch, seed, copy and commit. The copy span
//! records logical, scanned and copied bytes; identifiers never become tags.

use std::time::Instant;

use crate::emit::{counter, elapsed_ms, tag, timing_ms};

/// The rootfs device stack: dm-snapshot composite over an encrypted COW.
pub const OP_ROOTFS: &str = "rootfs";
/// The encrypted snapshot-capture volume a suspend writes into.
pub const OP_SNAPSHOT_VOLUME: &str = "snapshot_volume";
/// Retaining a pre-boot reflink of an ordinary warm-pool rootfs.
pub const OP_POOL_ROOTFS: &str = "pool_rootfs";

/// Includes unsuccessful attempts, so a fast success histogram cannot hide failures.
pub fn pool_adoption_duration(encrypted: bool, succeeded: bool, started: Instant) {
    timing_ms(
        "byok.pool_adoption.duration_ms",
        elapsed_ms(started),
        &[
            tag("encrypted", if encrypted { "true" } else { "false" }),
            tag("outcome", if succeeded { "success" } else { "failure" }),
        ],
    );
}

/// Kernel support for the required device-mapper targets is missing —
/// `dm_crypt` and/or `dm_snapshot` did not load. Node-level: this node
/// cannot run encrypted sandboxes at all until it is fixed, and every
/// encrypted placement landing on it will fail the same way.
pub const PHASE_KERNEL: &str = "kernel";
/// The volume DEK could not be obtained from credmgr. Control-plane
/// level: the authority is unreachable, or refused for this org.
pub const PHASE_DEK: &str = "dek";
/// Not enough free space to set up the device safely.
pub const PHASE_DISK: &str = "disk";
/// A caller-supplied parameter was unusable. A bug, not an operational
/// condition — re-asking will not help.
pub const PHASE_PARAMS: &str = "params";
/// A `cryptsetup`/`dmsetup`/`losetup`/`mkfs`/`mount` invocation failed, or
/// I/O around one did. The catch-all for real device trouble.
pub const PHASE_DEVICE: &str = "device";
/// Copying the sealed (ciphertext) artifacts out to the caller's publication
/// staging dir failed. Distinct from `device` because the volume itself
/// sealed correctly — what failed is handing the artifacts onward, which is
/// the step a suspend's durability depends on next.
pub const PHASE_STAGE: &str = "stage";

/// credmgr was not reachable at all (socket missing, peer down).
pub const DEK_UNREACHABLE: &str = "unreachable";
/// credmgr answered and refused: org not enrolled, KEK revoked, purpose
/// not permitted. Retrying without an operator change will keep failing.
pub const DEK_REFUSED: &str = "refused";
/// credmgr answered with something else — internal error, timeout.
pub const DEK_ERROR: &str = "error";

/// One encrypted device prepare was attempted.
///
/// Paired with [`device_prepare_failed`] so a failure *rate* is available
/// without needing a separate success counter: encrypted creates are rare
/// enough relative to plaintext ones that an absolute failure count says
/// little on its own.
pub fn device_prepare_attempted(op: &str) {
    counter("byok.device_prepare.attempted", 1, &[tag("op", op)]);
}

/// One encrypted device prepare failed, bucketed by `phase`.
pub fn device_prepare_failed(op: &str, phase: &str) {
    counter(
        "byok.device_prepare.failed",
        1,
        &[tag("op", op), tag("phase", phase)],
    );
}

/// Wall time of a successful encrypted device prepare.
///
/// This sits on the sandbox-start and restore critical paths — a LUKS
/// format plus a zero-prefix write is not free — so it is the latency an
/// encrypted sandbox pays over a plaintext one.
pub fn device_prepare_duration(op: &str, started: Instant) {
    timing_ms(
        "byok.device_prepare.duration_ms",
        elapsed_ms(started),
        &[tag("op", op)],
    );
}

/// One encrypted device release (seal + teardown) was attempted.
pub fn device_release_attempted(op: &str) {
    counter("byok.device_release.attempted", 1, &[tag("op", op)]);
}

/// One encrypted device release failed, bucketed by `phase`.
///
/// Release failures matter more than their rate suggests: the release is
/// what seals a suspend's ciphertext and stages it for upload, so a
/// failure here is adjacent to losing the snapshot rather than merely
/// failing an operation.
pub fn device_release_failed(op: &str, phase: &str) {
    counter(
        "byok.device_release.failed",
        1,
        &[tag("op", op), tag("phase", phase)],
    );
}

/// A volume-DEK fetch from credmgr was attempted.
pub fn dek_fetch_attempted() {
    counter("byok.dek_fetch.attempted", 1, &[]);
}

/// A volume-DEK fetch from credmgr failed.
pub fn dek_fetch_failed(reason: &str) {
    counter("byok.dek_fetch.failed", 1, &[tag("reason", reason)]);
}

/// Wall time of a successful volume-DEK fetch.
///
/// Worth watching on its own: the node opens a fresh connection per fetch
/// and the authority terminates every worker's key traffic, so this is
/// where authority saturation would first show up as sandbox-start
/// latency.
pub fn dek_fetch_duration(started: Instant) {
    timing_ms("byok.dek_fetch.duration_ms", elapsed_ms(started), &[]);
}
