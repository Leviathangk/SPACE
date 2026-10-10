use std::future::Future;
use std::pin::pin;

use futures_util::FutureExt;

use super::*;

/// Poll once without a waker. A pending semaphore acquire stays queued in
/// FIFO order, so tests order waiters by the order they first poll them.
fn poll<F: Future + Unpin>(future: F) -> Option<F::Output> {
    future.now_or_never()
}

#[tokio::test]
async fn high_priority_takes_whichever_pool_frees_first() {
    for reserve_frees_first in [false, true] {
        // 2 shared + 1 reserved.
        let admission = CodecAdmission::new(3, 1);
        let reserved = poll(pin!(admission.acquire(CodecPriority::High)))
            .unwrap()
            .unwrap();
        assert_eq!(admission.reserve_available(), 0, "high prefers the reserve");
        let shared = [
            poll(pin!(admission.acquire(CodecPriority::High)))
                .unwrap()
                .unwrap(),
            poll(pin!(admission.acquire(CodecPriority::Normal)))
                .unwrap()
                .unwrap(),
        ];
        assert_eq!(admission.shared_available(), 0);

        let mut waiter = pin!(admission.acquire(CodecPriority::High));
        assert!(poll(waiter.as_mut()).is_none());
        let [first_shared, second_shared] = shared;
        if reserve_frees_first {
            drop(reserved);
            let _admitted = poll(waiter.as_mut()).unwrap().unwrap();
            assert_eq!(admission.reserve_available(), 0);
            drop(first_shared);
            assert_eq!(admission.shared_available(), 1);
        } else {
            drop(first_shared);
            let _admitted = poll(waiter.as_mut()).unwrap().unwrap();
            assert_eq!(admission.shared_available(), 0);
            drop(reserved);
            assert_eq!(admission.reserve_available(), 1);
        }
        drop(second_shared);
    }
}

#[tokio::test]
async fn normal_priority_never_takes_the_reserve() {
    let admission = CodecAdmission::new(2, 1);
    let _shared = admission.acquire(CodecPriority::Normal).await.unwrap();
    let mut waiter = pin!(admission.acquire(CodecPriority::Normal));
    assert!(poll(waiter.as_mut()).is_none());
    assert!(admission.try_acquire_shared().is_none());
    assert_eq!(admission.reserve_available(), 1);
    // The reserve still admits high priority while normal work waits.
    let _high = poll(pin!(admission.acquire(CodecPriority::High)))
        .unwrap()
        .unwrap();
    assert!(poll(waiter.as_mut()).is_none());
}

#[tokio::test]
async fn reserve_is_capped_so_the_shared_pool_keeps_one_permit() {
    let admission = CodecAdmission::new(2, 5);
    assert!(admission.has_reserve());
    assert_eq!(admission.shared_available(), 1);
    assert_eq!(admission.reserve_available(), 1);
    assert!(!CodecAdmission::new(1, 1).has_reserve());
}

#[tokio::test]
async fn a_high_priority_waiter_does_not_jump_the_shared_queue() {
    let admission = CodecAdmission::new(2, 1);
    let shared = admission.acquire(CodecPriority::Normal).await.unwrap();
    let reserved = admission.acquire(CodecPriority::High).await.unwrap();
    let mut normal = pin!(admission.acquire(CodecPriority::Normal));
    let mut high = pin!(admission.acquire(CodecPriority::High));
    assert!(poll(normal.as_mut()).is_none());
    assert!(poll(high.as_mut()).is_none());

    // The high waiter queued on the shared pool behind the normal one.
    drop(shared);
    assert!(poll(high.as_mut()).is_none());
    let _normal = poll(normal.as_mut()).unwrap().unwrap();
    // Its own reserve still admits it.
    drop(reserved);
    let _high = poll(high.as_mut()).unwrap().unwrap();
    assert_eq!(admission.reserve_available(), 0);
}

#[tokio::test]
async fn a_high_priority_acquire_returns_the_pool_it_did_not_take() {
    let admission = CodecAdmission::new(2, 1);
    let shared = admission.acquire(CodecPriority::Normal).await.unwrap();
    let reserved = admission.acquire(CodecPriority::High).await.unwrap();
    let mut waiter = pin!(admission.acquire(CodecPriority::High));
    assert!(poll(waiter.as_mut()).is_none());
    // Both pools hand their permit to the waiter before it runs again.
    drop(shared);
    drop(reserved);
    let _admitted = poll(waiter.as_mut()).unwrap().unwrap();
    assert_eq!(
        (admission.shared_available(), admission.reserve_available()),
        (1, 0),
        "the reserve wins and the shared permit returns"
    );
}

#[tokio::test]
async fn cancelled_waiting_acquire_leaks_nothing() {
    let admission = CodecAdmission::new(2, 1);
    let reserved = admission.acquire(CodecPriority::High).await.unwrap();
    let shared = admission.acquire(CodecPriority::Normal).await.unwrap();
    {
        let mut waiter = pin!(admission.acquire(CodecPriority::High));
        assert!(poll(waiter.as_mut()).is_none());
        // Both pools hand their released permit to the queued waiter, which
        // is dropped before it observes either.
        drop(shared);
        drop(reserved);
        assert_eq!(admission.shared_available(), 0);
        assert_eq!(admission.reserve_available(), 0);
    }
    assert_eq!(admission.shared_available(), 1);
    assert_eq!(admission.reserve_available(), 1);

    let mut slot = admission.acquire_slot(CodecPriority::Normal).await.unwrap();
    slot.lend();
    let holder = admission.acquire(CodecPriority::Normal).await.unwrap();
    {
        let mut reacquire = pin!(slot.reacquire());
        assert!(poll(reacquire.as_mut()).is_none());
        drop(holder);
    }
    assert!(slot.is_lent(), "a cancelled reacquire leaves the slot lent");
    assert_eq!(admission.shared_available(), 1);
}

#[tokio::test]
async fn lending_frees_a_permit_and_reacquire_queues_behind_waiters() {
    let admission = CodecAdmission::new(1, 0);
    let mut slot = admission.acquire_slot(CodecPriority::Normal).await.unwrap();
    let mut first = pin!(admission.acquire(CodecPriority::Normal));
    assert!(poll(first.as_mut()).is_none());

    slot.lend();
    slot.lend();
    assert!(slot.is_lent());
    let first = poll(first.as_mut())
        .expect("lending hands the permit to the queued waiter")
        .unwrap();

    let mut second = pin!(admission.acquire(CodecPriority::Normal));
    assert!(poll(second.as_mut()).is_none());
    {
        let mut reacquire = pin!(slot.reacquire());
        assert!(poll(reacquire.as_mut()).is_none());

        drop(first);
        assert!(
            poll(reacquire.as_mut()).is_none(),
            "reacquire waits behind the earlier waiter"
        );
        let second = poll(second.as_mut()).unwrap().unwrap();
        drop(second);
        poll(reacquire.as_mut()).unwrap().unwrap();
    }
    assert!(!slot.is_lent());
    assert_eq!(admission.shared_available(), 0);
    poll(pin!(slot.reacquire())).unwrap().unwrap();
    drop(slot);
    assert_eq!(admission.shared_available(), 1);
}

#[tokio::test]
async fn a_lent_high_priority_slot_reacquires_from_the_reserve() {
    let admission = CodecAdmission::new(2, 1);
    let mut slot = admission.acquire_slot(CodecPriority::High).await.unwrap();
    assert_eq!(admission.reserve_available(), 0);
    slot.lend();
    // The shared pool fills while the slot is lent.
    let _shared = admission.acquire(CodecPriority::Normal).await.unwrap();
    poll(pin!(slot.reacquire()))
        .expect("a high-priority slot resumes from the reserve")
        .unwrap();
    assert!(!slot.is_lent());
    assert_eq!(admission.reserve_available(), 0);
}

fn max_threads(threads: u32) -> CompressMaxThreads {
    CompressMaxThreads::new(threads).unwrap()
}

#[tokio::test]
async fn compress_extras_come_only_from_idle_shared_permits() {
    // 3 shared + 3 reserved, up to 4 threads per pass.
    let admission = CodecAdmission::new(6, 3).with_compress_max_threads(max_threads(4));

    // A high-priority pass takes its primary from the reserve, but its
    // extras only from shared capacity, leaving one shared permit idle.
    let high = admission
        .acquire_compress(CodecPriority::High, CompressPath::File)
        .await
        .unwrap();
    assert_eq!(high.workers(), 2);
    assert_eq!(admission.reserve_available(), 2);
    assert_eq!(admission.shared_available(), 1);
    drop(high);
    assert_eq!(admission.shared_available(), 3);
    assert_eq!(admission.reserve_available(), 3);

    // A normal pass on an idle pool takes its primary from the shared pool,
    // so only one extra fits beside the spare. One worker is not worth a
    // permit that could admit a whole pass: it runs on its calling thread
    // and returns every permit it tried.
    let first = admission
        .acquire_compress(CodecPriority::Normal, CompressPath::File)
        .await
        .unwrap();
    assert_eq!(first.workers(), 0);
    assert_eq!(admission.shared_available(), 2);
    let second = admission
        .acquire_compress(CodecPriority::Normal, CompressPath::File)
        .await
        .unwrap();
    assert_eq!(second.workers(), 0);
    assert_eq!(admission.shared_available(), 1);
    // With the shared pool exhausted, a protected pass still starts from the
    // reserve, and nothing is ever borrowed from the reserve for extras.
    let protected = admission
        .acquire_compress(CodecPriority::High, CompressPath::File)
        .await
        .unwrap();
    assert_eq!(protected.workers(), 0);
    assert_eq!(admission.reserve_available(), 2);

    drop((first, second, protected));
    assert_eq!(admission.shared_available(), 3);
    assert_eq!(admission.reserve_available(), 3);
}

#[tokio::test]
async fn a_pass_runs_two_workers_or_none() {
    let admission = CodecAdmission::new(8, 0).with_compress_max_threads(max_threads(4));
    for path in [
        CompressPath::File,
        CompressPath::Streaming,
        CompressPath::BtrfsSend,
    ] {
        for (idle, workers) in [(2, 0), (3, 0), (4, 2), (5, 3)] {
            let held: Vec<_> = (0..8 - idle)
                .map(|_| admission.try_acquire_shared().unwrap())
                .collect();
            let grant = admission
                .acquire_compress(CodecPriority::Normal, path)
                .await
                .unwrap();
            assert_eq!(grant.workers(), workers, "{path:?} with {idle} idle");
            assert_eq!(
                admission.shared_available(),
                idle - 1 - workers as usize,
                "{path:?} with {idle} idle: only the granted extras stay held"
            );
            assert_eq!(
                admission.idle_capable_extras.available_permits(),
                if path.may_idle_extras() {
                    3 - workers as usize
                } else {
                    3
                },
                "{path:?} with {idle} idle: only the granted extras keep tokens"
            );
            drop((grant, held));
            assert_eq!(admission.shared_available(), 8);
        }
    }
}

#[tokio::test]
async fn contended_compress_passes_run_single_threaded() {
    let admission = CodecAdmission::new(4, 0).with_compress_max_threads(max_threads(4));
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(admission.acquire(CodecPriority::Normal).await.unwrap());
    }
    let mut first = pin!(admission.acquire_compress(CodecPriority::Normal, CompressPath::File));
    let mut second = pin!(admission.acquire_compress(CodecPriority::Normal, CompressPath::File));
    assert!(poll(first.as_mut()).is_none());
    assert!(poll(second.as_mut()).is_none());
    // Two permits free up while two passes wait: each waiter receives one,
    // and neither finds idle capacity for extras.
    held.truncate(2);
    let first = poll(first.as_mut()).unwrap().unwrap();
    let second = poll(second.as_mut()).unwrap().unwrap();
    assert_eq!((first.workers(), second.workers()), (0, 0));
    drop((first, second, held));
    assert_eq!(admission.shared_available(), 4);

    // The kill switch keeps even an idle pool single-threaded.
    let admission = CodecAdmission::new(4, 0).with_compress_max_threads(max_threads(1));
    let grant = admission
        .acquire_compress(CodecPriority::Normal, CompressPath::File)
        .await
        .unwrap();
    assert_eq!(grant.workers(), 0);
    assert_eq!(admission.shared_available(), 3);
}

#[tokio::test]
async fn a_grant_counts_its_calling_thread() {
    for max in [1, 3, 4] {
        let admission = CodecAdmission::new(8, 0).with_compress_max_threads(max_threads(max));
        for path in [
            CompressPath::File,
            CompressPath::Streaming,
            CompressPath::BtrfsSend,
        ] {
            let grant = admission
                .acquire_compress(CodecPriority::Normal, path)
                .await
                .unwrap();
            let held = 8 - admission.shared_available();
            assert_eq!(held, max as usize, "an idle pool grants the full pass");
            assert_eq!(
                grant.workers() as usize,
                held - 1,
                "the primary backs the calling thread, each extra one worker"
            );
        }
    }
}

#[tokio::test]
async fn encrypted_passes_take_a_single_permit() {
    assert_eq!(
        extra_permits_wanted(CompressPath::Encrypted, max_threads(4)),
        0
    );
    let admission = CodecAdmission::new(8, 2).with_compress_max_threads(max_threads(4));
    for priority in [CodecPriority::Normal, CodecPriority::High] {
        let grant = admission
            .acquire_compress(priority, CompressPath::Encrypted)
            .await
            .unwrap();
        assert_eq!(grant.workers(), 0);
        assert_eq!(
            admission.shared_available() + admission.reserve_available(),
            7,
            "an idle pool still admits the encrypted pass on its primary alone"
        );
    }
}

#[tokio::test]
async fn a_lent_primary_is_never_counted_as_spare() {
    let admission = CodecAdmission::new(8, 0).with_compress_max_threads(max_threads(4));
    let mut producer = admission
        .acquire_compress(CodecPriority::Normal, CompressPath::Streaming)
        .await
        .unwrap();
    assert_eq!(producer.workers(), 3);
    assert_eq!(admission.shared_available(), 4);
    // Backpressure: the producer lends its primary and will take it back.
    producer.lend();
    assert_eq!(admission.shared_available(), 5);

    // The next pass may not take the lent permit, nor the one spare.
    let next = admission
        .acquire_compress(CodecPriority::Normal, CompressPath::File)
        .await
        .unwrap();
    assert_eq!(next.workers(), 2);
    assert_eq!(admission.shared_available(), 2);
    // So the producer resumes at once, and the spare still admits a pass.
    poll(pin!(producer.reacquire())).unwrap().unwrap();
    let last = poll(pin!(
        admission.acquire_compress(CodecPriority::Normal, CompressPath::File)
    ))
    .expect("the spare permit admits the next pass without queueing")
    .unwrap();
    assert_eq!(last.workers(), 0);
    drop((producer, next, last));
    assert_eq!(admission.shared_available(), 8);
    assert_eq!(admission.lent.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn simultaneous_admissions_keep_the_spare() {
    // Both primaries are admitted before either pass looks for extras, so
    // only five extras fit beside the spare: [3, 2], never [3, 3].
    let admission = CodecAdmission::new(8, 0).with_compress_max_threads(max_threads(4));
    let held: Vec<_> = (0..8)
        .map(|_| admission.try_acquire_shared().unwrap())
        .collect();
    let mut first = pin!(admission.acquire_compress(CodecPriority::Normal, CompressPath::File));
    let mut second = pin!(admission.acquire_compress(CodecPriority::Normal, CompressPath::File));
    assert!(poll(first.as_mut()).is_none());
    assert!(poll(second.as_mut()).is_none());
    drop(held);
    let first = poll(first.as_mut()).unwrap().unwrap();
    let second = poll(second.as_mut()).unwrap().unwrap();
    assert_eq!((first.workers(), second.workers()), (3, 2));
    assert_eq!(admission.shared_available(), 1);
}

#[test]
fn parallel_admissions_keep_the_spare() {
    // The same race with the extras step on two OS threads at once. The
    // grab is atomic, so every round leaves the spare idle. A pass that tries
    // while the other holds its surplus sees too few idle permits and runs
    // single-threaded, so a round can leave more than the spare.
    let admission = CodecAdmission::new(8, 0).with_compress_max_threads(max_threads(4));
    let barrier = std::sync::Barrier::new(2);
    for _ in 0..2_000 {
        let held: Vec<_> = (0..8)
            .map(|_| admission.try_acquire_shared().unwrap())
            .collect();
        let mut waiters: Vec<_> = (0..2)
            .map(|_| {
                Box::pin(admission.acquire_compress(CodecPriority::Normal, CompressPath::File))
            })
            .collect();
        for waiter in &mut waiters {
            assert!(poll(waiter.as_mut()).is_none());
        }
        drop(held);
        let grants: Vec<_> = std::thread::scope(|scope| {
            let threads: Vec<_> = waiters
                .into_iter()
                .map(|mut waiter| {
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        poll(waiter.as_mut()).unwrap().unwrap()
                    })
                })
                .collect();
            threads.into_iter().map(|t| t.join().unwrap()).collect()
        });
        assert!(
            admission.shared_available() >= 1,
            "workers {:?}",
            grants
                .iter()
                .map(CompressGrant::workers)
                .collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn streaming_extras_are_capped_node_wide() {
    let admission = CodecAdmission::new(16, 0).with_compress_max_threads(max_threads(4));
    let grant = |path| admission.acquire_compress(CodecPriority::Normal, path);
    let first = grant(CompressPath::Streaming).await.unwrap();
    assert_eq!(first.workers(), 3);
    // Plenty of idle permits, but a second streaming producer could hold
    // its extras idle too, so it runs on its calling thread.
    let second = grant(CompressPath::Streaming).await.unwrap();
    assert_eq!(second.workers(), 0);
    // CPU-bound temp-file passes stay uncapped.
    let file = grant(CompressPath::File).await.unwrap();
    let send = grant(CompressPath::BtrfsSend).await.unwrap();
    assert_eq!((file.workers(), send.workers()), (3, 3));
    drop(first);
    let third = grant(CompressPath::Streaming).await.unwrap();
    assert_eq!(third.workers(), 3);
    drop((second, third, file, send));

    // A producer granted fewer extras than its cap keeps only those tokens,
    // so the rest of the cap stays available.
    let held: Vec<_> = (0..12)
        .map(|_| admission.try_acquire_shared().unwrap())
        .collect();
    let partial = grant(CompressPath::Streaming).await.unwrap();
    assert_eq!(partial.workers(), 2);
    assert_eq!(admission.idle_capable_extras.available_permits(), 1);
    drop(held);
    // One token cannot back the two workers a multithreaded pass runs, so
    // with plenty of idle permits the next producer still runs on its
    // calling thread and leaves the token.
    let rest = grant(CompressPath::Streaming).await.unwrap();
    assert_eq!(rest.workers(), 0);
    assert_eq!(admission.idle_capable_extras.available_permits(), 1);
    drop((partial, rest));
    assert_eq!(admission.idle_capable_extras.available_permits(), 3);
}

#[tokio::test]
async fn grants_release_everything_on_drop_cancellation_and_panic() {
    let admission = CodecAdmission::new(6, 1).with_compress_max_threads(max_threads(4));
    let idle = |admission: &CodecAdmission| {
        (
            admission.shared_available(),
            admission.reserve_available(),
            admission.idle_capable_extras.available_permits(),
            admission.lent.load(Ordering::SeqCst),
        )
    };
    assert_eq!(idle(&admission), (5, 1, 3, 0));

    // Drop, including a lent producer.
    let mut producer = admission
        .acquire_compress(CodecPriority::High, CompressPath::Streaming)
        .await
        .unwrap();
    assert_eq!(producer.workers(), 3);
    producer.lend();
    drop(producer);
    assert_eq!(idle(&admission), (5, 1, 3, 0));

    // Cancellation: of a queued admission and of a queued reacquire.
    let mut producer = admission
        .acquire_compress(CodecPriority::Normal, CompressPath::Streaming)
        .await
        .unwrap();
    let held: Vec<_> = std::iter::from_fn(|| admission.try_acquire_shared()).collect();
    producer.lend();
    let blocker = admission.try_acquire_shared().unwrap();
    {
        let mut queued =
            pin!(admission.acquire_compress(CodecPriority::Normal, CompressPath::File));
        assert!(poll(queued.as_mut()).is_none());
        let mut reacquire = pin!(producer.reacquire());
        assert!(poll(reacquire.as_mut()).is_none());
        drop(blocker);
    }
    assert!(producer.is_lent());
    drop((producer, held));
    assert_eq!(idle(&admission), (5, 1, 3, 0));

    // Panic in the blocking work that owns the grant.
    let grant = admission
        .acquire_compress(CodecPriority::Normal, CompressPath::Streaming)
        .await
        .unwrap();
    let panicked = std::thread::spawn(move || {
        let _grant = grant;
        panic!("codec work failed");
    })
    .join();
    assert!(panicked.is_err());
    assert_eq!(idle(&admission), (5, 1, 3, 0));
}

/// Reproducible xorshift for the model walk; a property-test dependency
/// would add little to one seeded state machine.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % u64::try_from(n).unwrap()).unwrap()
    }
}

enum Held {
    Permit(CodecPermit),
    Grant(CompressGrant, CompressPath),
}

/// A pending future, with the extras of the lent streaming grant it owns
/// when it is a reacquire.
type Pending<'a> = (
    Option<usize>,
    std::pin::Pin<Box<dyn Future<Output = Held> + 'a>>,
);

/// Random walk over acquire, compress grant, lend, reacquire and drop across
/// simulated holders. Every pending future is polled to a fixpoint after each
/// step, so every permit is either held by an observed holder or available.
/// No runtime: Tokio's cooperative budget would make long poll loops inside a
/// task return spurious `Pending`.
#[test]
fn model_walk_keeps_admission_invariants() {
    const PATHS: [CompressPath; 4] = [
        CompressPath::File,
        CompressPath::Streaming,
        CompressPath::BtrfsSend,
        CompressPath::Encrypted,
    ];
    for seed in 1..=64u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let total = 1 + rng.below(10);
        let reserve = rng.below(4);
        let max = [1, 3, 4, 5, 6][rng.below(5)];
        let admission =
            CodecAdmission::new(total, reserve).with_compress_max_threads(max_threads(max));
        let shared_total = admission.shared_available();
        let reserve_total = admission.reserve_available();
        let cap = usize::try_from(max - 1).unwrap();
        let mut held: Vec<Held> = Vec::new();
        let mut pending: Vec<Pending<'_>> = Vec::new();
        let priority = |rng: &mut Rng| {
            if rng.below(2) == 0 {
                CodecPriority::Normal
            } else {
                CodecPriority::High
            }
        };
        for step in 0..2_000 {
            match rng.below(6) {
                0 => {
                    let priority = priority(&mut rng);
                    let admission = &admission;
                    pending.push((
                        None,
                        Box::pin(async move {
                            Held::Permit(admission.acquire(priority).await.unwrap())
                        }),
                    ));
                }
                1 => {
                    let priority = priority(&mut rng);
                    let path = PATHS[rng.below(PATHS.len())];
                    let admission = &admission;
                    pending.push((
                        None,
                        Box::pin(async move {
                            Held::Grant(
                                admission.acquire_compress(priority, path).await.unwrap(),
                                path,
                            )
                        }),
                    ));
                }
                2 => {
                    // Producers lend only on the streaming path.
                    let producers: Vec<_> = held
                        .iter_mut()
                        .filter_map(|held| match held {
                            Held::Grant(grant, CompressPath::Streaming) => Some(grant),
                            _ => None,
                        })
                        .collect();
                    if !producers.is_empty() {
                        let n = rng.below(producers.len());
                        producers.into_iter().nth(n).unwrap().lend();
                    }
                }
                3 => {
                    let lent: Vec<_> = (0..held.len())
                        .filter(|&i| matches!(&held[i], Held::Grant(grant, _) if grant.is_lent()))
                        .collect();
                    if !lent.is_empty() {
                        let Held::Grant(mut grant, path) =
                            held.swap_remove(lent[rng.below(lent.len())])
                        else {
                            unreachable!()
                        };
                        let extras = grant
                            ._extras
                            .as_ref()
                            .map_or(0, OwnedSemaphorePermit::num_permits);
                        pending.push((
                            Some(extras),
                            Box::pin(async move {
                                grant.reacquire().await.unwrap();
                                Held::Grant(grant, path)
                            }),
                        ));
                    }
                }
                4 if !held.is_empty() => drop(held.swap_remove(rng.below(held.len()))),
                5 if !pending.is_empty() => drop(pending.swap_remove(rng.below(pending.len()))),
                _ => {}
            }
            loop {
                let before = pending.len();
                let mut index = 0;
                while index < pending.len() {
                    if let Some(ready) = poll(pending[index].1.as_mut()) {
                        held.push(ready);
                        drop(pending.swap_remove(index));
                    } else {
                        index += 1;
                    }
                }
                if pending.len() == before {
                    break;
                }
            }

            let from_shared =
                |permit: &OwnedSemaphorePermit| Arc::ptr_eq(permit.semaphore(), &admission.shared);
            let (mut shared_held, mut reserve_held, mut streaming_extras, mut lent) = (0, 0, 0, 0);
            for held in &held {
                let (primary, grant) = match held {
                    Held::Permit(permit) => (Some(permit), None),
                    Held::Grant(grant, path) => match &grant.primary.state {
                        SlotState::Held { _permit } => (Some(_permit), Some((grant, path))),
                        SlotState::Lent { .. } => {
                            lent += 1;
                            (None, Some((grant, path)))
                        }
                    },
                };
                if let Some(primary) = primary {
                    if from_shared(&primary._permit) {
                        shared_held += 1;
                    } else {
                        reserve_held += 1;
                    }
                }
                let Some((grant, path)) = grant else { continue };
                let extras = grant
                    ._extras
                    .as_ref()
                    .map_or(0, OwnedSemaphorePermit::num_permits);
                assert!(
                    grant._extras.as_ref().is_none_or(from_shared),
                    "seed {seed} step {step}: reserve permits never back extras"
                );
                shared_held += extras;
                assert_eq!(
                    usize::try_from(grant.workers()).unwrap(),
                    extras,
                    "seed {seed} step {step}: every worker is backed by an extra, lent or not"
                );
                assert!(
                    grant.workers() == 0 || grant.workers() >= MIN_WORKERS,
                    "seed {seed} step {step}: a pass runs two workers or none"
                );
                let tokens = grant
                    ._idle_capable_tokens
                    .as_ref()
                    .map_or(0, OwnedSemaphorePermit::num_permits);
                if path.may_idle_extras() {
                    assert_eq!(tokens, extras);
                    streaming_extras += extras;
                } else {
                    assert_eq!(tokens, 0);
                }
                if *path == CompressPath::Encrypted {
                    assert_eq!(extras, 0);
                }
            }
            for extras in pending.iter().filter_map(|(reacquire, _)| *reacquire) {
                lent += 1;
                shared_held += extras;
                streaming_extras += extras;
            }
            assert_eq!(
                (
                    shared_held + admission.shared_available(),
                    reserve_held + admission.reserve_available()
                ),
                (shared_total, reserve_total),
                "seed {seed} step {step}: every permit is held or available"
            );
            assert!(
                streaming_extras <= cap,
                "seed {seed} step {step}: streaming cap"
            );
            assert_eq!(
                streaming_extras + admission.idle_capable_extras.available_permits(),
                cap
            );
            assert_eq!(
                admission.lent.load(Ordering::SeqCst),
                lent,
                "seed {seed} step {step}"
            );
        }
        drop(pending);
        drop(held);
        assert_eq!(
            (
                admission.shared_available(),
                admission.reserve_available(),
                admission.idle_capable_extras.available_permits(),
                admission.lent.load(Ordering::SeqCst),
            ),
            (shared_total, reserve_total, cap, 0),
            "seed {seed}: everything is returned"
        );
    }
}
