//! Admission for CPU-bound codec passes (zstd compress or decompress).
//!
//! One pool of permits, one core each, bounds how many passes run at once.
//! A small part of it can be reserved for high-priority passes: they race the
//! reserve and the shared pool and take whichever frees first, while normal
//! passes only ever draw on the shared pool. Tokio semaphores are FIFO and
//! hand released permits to queued waiters first, so admission starts passes
//! in arrival order within each pool; it does not finish them in order.
//!
//! Every permit is an RAII value. Callers normally move it into the blocking
//! closure that runs the CPU work, so it releases only when that work exits,
//! also when the async caller is cancelled. The exceptions: a sparse-shadow
//! comparison holds its permit in the async task around its `spawn_blocking`,
//! a streaming producer's slot is lent while the producer is blocked on its
//! output, and a streamed pull's decode guard is held by the object-store
//! download path.
//!
//! An upload compression pass ([`CodecAdmission::acquire_compress`]) takes
//! its primary permit exactly like any other pass, then may top up with
//! extra permits, taken without waiting and only from idle shared capacity.
//! The primary backs the calling thread, which reads input, copies it into
//! zstd jobs and writes output; each extra backs one zstd worker.

use std::num::NonZeroU32;
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Which pools a codec pass may draw on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecPriority {
    /// Shared pool only.
    Normal,
    /// Reserve or shared pool, whichever frees first.
    High,
}

/// Most codec permits one upload compression pass may hold, counting the
/// calling thread. A pass holding N permits runs N - 1 zstd workers beside
/// the calling thread. Every permit past the first is an extra taken only
/// from idle shared capacity, so contended passes keep running on the
/// calling thread alone. `1`, the default, keeps compression synchronous.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompressMaxThreads(NonZeroU32);

impl CompressMaxThreads {
    pub const SINGLE: Self = Self(NonZeroU32::MIN);

    pub const fn new(threads: NonZeroU32) -> Self {
        Self(threads)
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl Default for CompressMaxThreads {
    fn default() -> Self {
        Self::SINGLE
    }
}

/// What an upload compression pass feeds, which decides how many extra
/// permits it may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompressPath {
    /// A staged file compressed to a temporary file.
    File,
    /// A producer streaming into a bounded multipart upload. It lends its
    /// primary permit while the upload applies backpressure.
    Streaming,
    /// A btrfs send stream compressed to a temporary file.
    BtrfsSend,
    /// A staged file compressed and encrypted to a temporary file.
    Encrypted,
}

/// Extra permits a pass on `path` asks for beyond its primary.
fn extra_permits_wanted(path: CompressPath, max_threads: CompressMaxThreads) -> u32 {
    match path {
        CompressPath::File
        | CompressPath::Streaming
        | CompressPath::BtrfsSend
        | CompressPath::Encrypted => max_threads.get() - 1,
    }
}

/// The one way admission can fail: the permit pool closed. Admission never
/// closes its semaphores, so this is unreachable while the owner lives.
#[derive(Debug, thiserror::Error)]
#[error("codec permit pool closed")]
pub struct CodecAdmissionClosed;

/// Shared and reserved codec permit pools. `Clone` shares the pools.
#[derive(Clone)]
pub struct CodecAdmission {
    shared: Arc<Semaphore>,
    reserve: Arc<Semaphore>,
    reserved: usize,
    compress_max_threads: CompressMaxThreads,
}

impl CodecAdmission {
    /// `total` permits, of which `reserve` are kept for high-priority passes.
    /// The reserve is capped at `total - 1` so the shared pool keeps at least
    /// one permit for normal work. Codec capacity is CPU-bound, so the
    /// reserve is carved from the total rather than added to it.
    pub fn new(total: usize, reserve: usize) -> Self {
        let reserve = reserve.min(total.saturating_sub(1));
        Self {
            shared: Arc::new(Semaphore::new(total - reserve)),
            reserve: Arc::new(Semaphore::new(reserve)),
            reserved: reserve,
            compress_max_threads: CompressMaxThreads::SINGLE,
        }
    }

    /// Let upload compression passes hold up to `max` permits.
    pub fn with_compress_max_threads(mut self, max: CompressMaxThreads) -> Self {
        self.compress_max_threads = max;
        self
    }

    /// Whether any permits are reserved. Without a reserve,
    /// [`CodecPriority::High`] behaves exactly like
    /// [`CodecPriority::Normal`].
    pub fn has_reserve(&self) -> bool {
        self.reserved > 0
    }

    /// Wait for one permit. See [`CodecPriority`] for which pools it may
    /// come from. Cancel-safe: dropping the future returns any permit
    /// assigned to it.
    pub async fn acquire(
        &self,
        priority: CodecPriority,
    ) -> Result<CodecPermit, CodecAdmissionClosed> {
        let permit = match priority {
            CodecPriority::High => tokio::select! {
                biased;
                permit = Arc::clone(&self.reserve).acquire_owned() => permit,
                permit = Arc::clone(&self.shared).acquire_owned() => permit,
            },
            CodecPriority::Normal => Arc::clone(&self.shared).acquire_owned().await,
        };
        permit
            .map(|permit| CodecPermit { _permit: permit })
            .map_err(|_| CodecAdmissionClosed)
    }

    /// [`Self::acquire`] for a pass that may later lend its permit while it
    /// cannot run, such as a producer blocked on a full output pipe.
    pub async fn acquire_slot(
        &self,
        priority: CodecPriority,
    ) -> Result<CodecSlot, CodecAdmissionClosed> {
        let permit = self.acquire(priority).await?;
        Ok(CodecSlot {
            admission: self.clone(),
            priority,
            permit: Some(permit),
        })
    }

    /// Admit one upload compression pass on `path`: a primary permit as
    /// [`Self::acquire_slot`] takes it, then up to `compress_max_threads - 1`
    /// extras without waiting. Extras come only from the shared pool, so the
    /// reserve stays whole for protected restores, and one idle shared permit
    /// is left for the next pass to arrive. Tokio hands released permits
    /// straight to queued waiters, so a pool with a backlog has no idle
    /// permits and a contended pass runs single-threaded. The idle check and
    /// the grab are not atomic; a concurrent admission can occasionally leave
    /// the pool without its spare permit, which is still within its total.
    pub async fn acquire_compress(
        &self,
        priority: CodecPriority,
        path: CompressPath,
    ) -> Result<CompressGrant, CodecAdmissionClosed> {
        let primary = self.acquire_slot(priority).await?;
        let wanted = extra_permits_wanted(path, self.compress_max_threads);
        let spare = self.shared.available_permits().saturating_sub(1);
        let grant = wanted.min(u32::try_from(spare).unwrap_or(u32::MAX));
        let extras = (grant > 0)
            .then(|| Arc::clone(&self.shared).try_acquire_many_owned(grant).ok())
            .flatten();
        // The primary backs the calling thread; each extra backs a worker.
        let workers = extras.as_ref().map_or(0, |extras| {
            u32::try_from(extras.num_permits()).expect("extras fit the requested u32")
        });
        Ok(CompressGrant {
            primary,
            _extras: extras,
            workers,
        })
    }

    /// One shared permit if it is free right now, without queueing. Opportunistic
    /// work uses this so it never competes with queued passes or the reserve.
    pub fn try_acquire_shared(&self) -> Option<CodecPermit> {
        Arc::clone(&self.shared)
            .try_acquire_owned()
            .ok()
            .map(|permit| CodecPermit { _permit: permit })
    }

    #[cfg(test)]
    pub fn shared_available(&self) -> usize {
        self.shared.available_permits()
    }

    #[cfg(test)]
    pub fn reserve_available(&self) -> usize {
        self.reserve.available_permits()
    }
}

/// One admitted codec pass. Dropping it releases the permit.
#[must_use = "dropping a codec permit releases its admission"]
pub struct CodecPermit {
    _permit: OwnedSemaphorePermit,
}

/// A codec permit its holder can lend while it cannot run and must
/// reacquire before running again.
#[must_use = "dropping a codec slot releases its admission"]
pub struct CodecSlot {
    admission: CodecAdmission,
    priority: CodecPriority,
    /// `None` while lent.
    permit: Option<CodecPermit>,
}

impl CodecSlot {
    /// Release the permit to queued work. Idempotent.
    pub fn lend(&mut self) {
        self.permit = None;
    }

    pub fn is_lent(&self) -> bool {
        self.permit.is_none()
    }

    /// Take a permit again if lent, queueing FIFO behind passes already
    /// waiting. Cancel-safe: a cancelled reacquire leaves the slot lent.
    pub async fn reacquire(&mut self) -> Result<(), CodecAdmissionClosed> {
        if self.permit.is_none() {
            self.permit = Some(self.admission.acquire(self.priority).await?);
        }
        Ok(())
    }
}

/// Codec admission held by one upload compression pass. Its zstd worker
/// count is fixed when the encoder starts, so the extras are held until the
/// pass ends; only the primary, which backs the calling thread, can be lent.
pub struct CompressGrant {
    primary: CodecSlot,
    _extras: Option<OwnedSemaphorePermit>,
    workers: u32,
}

impl CompressGrant {
    /// zstd workers this grant pays for, one per extra permit. `0` means
    /// compress synchronously on the calling thread.
    pub fn workers(&self) -> u32 {
        self.workers
    }

    /// Lend the primary permit; see [`CodecSlot::lend`].
    pub fn lend(&mut self) {
        self.primary.lend();
    }

    pub fn is_lent(&self) -> bool {
        self.primary.is_lent()
    }

    /// Reacquire the primary permit; see [`CodecSlot::reacquire`].
    pub async fn reacquire(&mut self) -> Result<(), CodecAdmissionClosed> {
        self.primary.reacquire().await
    }
}

/// A single-threaded pass, for work that admits a codec pass without zstd.
impl From<CodecSlot> for CompressGrant {
    fn from(primary: CodecSlot) -> Self {
        Self {
            primary,
            _extras: None,
            workers: 0,
        }
    }
}

#[cfg(test)]
mod tests;
