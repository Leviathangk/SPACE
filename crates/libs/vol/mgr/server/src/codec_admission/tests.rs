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
    assert!(!CodecAdmission::new(3, 0).has_reserve());
}

#[tokio::test]
async fn waiters_are_admitted_in_arrival_order() {
    let admission = CodecAdmission::new(1, 0);
    let held = admission.acquire(CodecPriority::Normal).await.unwrap();
    let mut first = pin!(admission.acquire(CodecPriority::High));
    let mut second = pin!(admission.acquire(CodecPriority::Normal));
    let mut third = pin!(admission.acquire(CodecPriority::Normal));
    assert!(poll(first.as_mut()).is_none());
    assert!(poll(second.as_mut()).is_none());
    assert!(poll(third.as_mut()).is_none());

    drop(held);
    // Polling a later waiter first cannot jump the queue.
    assert!(poll(third.as_mut()).is_none());
    assert!(poll(second.as_mut()).is_none());
    let first = poll(first.as_mut()).unwrap().unwrap();
    drop(first);
    assert!(poll(third.as_mut()).is_none());
    let second = poll(second.as_mut()).unwrap().unwrap();
    drop(second);
    let _third = poll(third.as_mut()).unwrap().unwrap();
}

#[tokio::test]
async fn permits_release_on_drop() {
    let admission = CodecAdmission::new(4, 1);
    let permits = [
        admission.acquire(CodecPriority::High).await.unwrap(),
        admission.acquire(CodecPriority::Normal).await.unwrap(),
        admission.try_acquire_shared().unwrap(),
    ];
    let slot = admission.acquire_slot(CodecPriority::High).await.unwrap();
    assert!(
        poll(pin!(admission.acquire_slot(CodecPriority::High))).is_none(),
        "every permit is held"
    );
    assert_eq!(
        (admission.shared_available(), admission.reserve_available()),
        (0, 0)
    );
    drop(permits);
    drop(slot);
    assert_eq!(
        (admission.shared_available(), admission.reserve_available()),
        (3, 1)
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
