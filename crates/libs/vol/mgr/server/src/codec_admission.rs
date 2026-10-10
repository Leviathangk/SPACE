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
use std::sync::atomic::{AtomicUsize, Ordering};

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
///
/// The encrypted pass pulls compressed bytes through zstd's read-mode
/// encoder, which keeps only a job or two in flight and blocks each read on
/// the oldest job, so extra workers buy little, data-dependent parallelism
/// there while holding permits and job buffers. It always runs on its
/// calling thread.
fn extra_permits_wanted(path: CompressPath, max_threads: CompressMaxThreads) -> u32 {
    match path {
        CompressPath::File | CompressPath::Streaming | CompressPath::BtrfsSend => {
            max_threads.get() - 1
        }
        CompressPath::Encrypted => 0,
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
    /// Slots whose permit is lent and that will reacquire one before they run
    /// again. Their permits sit in the pools but are not spare.
    lent: Arc<AtomicUsize>,
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
            lent: Arc::new(AtomicUsize::new(0)),
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
            state: SlotState::Held { _permit: permit },
        })
    }

    /// Admit one upload compression pass on `path`: a primary permit as
    /// [`Self::acquire_slot`] takes it, then up to `compress_max_threads - 1`
    /// extras without waiting (see [`Self::try_acquire_extras`]). Extras come
    /// only from the shared pool, so the reserve stays whole for protected
    /// restores. Tokio hands released permits straight to queued waiters, so
    /// a pool with a backlog has no idle permits and a contended pass runs
    /// single-threaded.
    pub async fn acquire_compress(
        &self,
        priority: CodecPriority,
        path: CompressPath,
    ) -> Result<CompressGrant, CodecAdmissionClosed> {
        let primary = self.acquire_slot(priority).await?;
        let wanted = extra_permits_wanted(path, self.compress_max_threads);
        let extras = self.try_acquire_extras(wanted);
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

    /// Up to `wanted` shared permits, taken only if the pool keeps one idle
    /// permit for the next pass to arrive plus one for every lent slot,
    /// which reacquires before it runs again. A lent permit sits in the pool
    /// but is not spare: extras taken from it would idle beside a producer
    /// that is only waiting to resume.
    ///
    /// There is no check-then-act window: each attempt takes the extras and
    /// the permits to keep idle in one `try_acquire_many`, from the largest
    /// grant down, then returns the kept surplus at once. Concurrent
    /// admissions therefore cannot both count the same idle permit. The lent
    /// count is read before that attempt and may change while it runs. A
    /// slot that reacquires decrements the count only after its permit is
    /// taken, so a stale count only keeps more idle. A slot lent meanwhile
    /// increments the count before releasing its permit, so a lend that
    /// released a permit this attempt could take shows up in the second read,
    /// and one extra is returned per new lend. Either way the error is fewer
    /// extras, never more. While the surplus is held, a concurrent admission
    /// sees fewer idle permits and also takes fewer. Lent slots are counted
    /// whichever pool their permit came from; a lent reserve permit returns
    /// to the reserve, so counting it here also only keeps more idle.
    fn try_acquire_extras(&self, wanted: u32) -> Option<OwnedSemaphorePermit> {
        let lent = self.lent.load(Ordering::SeqCst);
        let keep_idle = u32::try_from(lent).ok()?.checked_add(1)?;
        for extras in (1..=wanted).rev() {
            let Ok(mut taken) =
                Arc::clone(&self.shared).try_acquire_many_owned(extras.checked_add(keep_idle)?)
            else {
                continue;
            };
            let newly_lent = self.lent.load(Ordering::SeqCst).saturating_sub(lent);
            let extras = usize::try_from(extras).ok()?.saturating_sub(newly_lent);
            // Dropping `taken` returns the permits kept idle.
            return (extras > 0).then(|| taken.split(extras)).flatten();
        }
        None
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
    state: SlotState,
}

enum SlotState {
    Held { _permit: CodecPermit },
    Lent { _counted: LentSlot },
}

/// Counts one lent slot in [`CodecAdmission`] for as long as it lives.
struct LentSlot(Arc<AtomicUsize>);

impl LentSlot {
    fn new(lent: &Arc<AtomicUsize>) -> Self {
        lent.fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(lent))
    }
}

impl Drop for LentSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl CodecSlot {
    /// Release the permit to queued work. Idempotent. The slot counts as lent
    /// before the permit is released, so extras are never granted from it.
    pub fn lend(&mut self) {
        if let SlotState::Held { .. } = self.state {
            // The new state is built, counting the lend, before the old one,
            // holding the permit, drops.
            self.state = SlotState::Lent {
                _counted: LentSlot::new(&self.admission.lent),
            };
        }
    }

    pub fn is_lent(&self) -> bool {
        matches!(self.state, SlotState::Lent { .. })
    }

    /// Take a permit again if lent, queueing FIFO behind passes already
    /// waiting. Cancel-safe: a cancelled reacquire leaves the slot lent. The
    /// slot stops counting as lent only once it holds its permit again.
    pub async fn reacquire(&mut self) -> Result<(), CodecAdmissionClosed> {
        if let SlotState::Lent { .. } = self.state {
            self.state = SlotState::Held {
                _permit: self.admission.acquire(self.priority).await?,
            };
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
