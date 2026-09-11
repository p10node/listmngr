use super::{Outcome, renewing};
use std::cell::Cell;
use std::time::Duration;

#[tokio::test(start_paused = true)]
async fn short_lease_is_renewed_through_work_longer_than_its_ttl() {
    let renewals = Cell::new(0);
    let start = tokio::time::Instant::now();
    let result = renewing(
        150,
        async {
            assert_eq!(renewals.get(), 1, "ownership checked before work");
            tokio::time::sleep(Duration::from_millis(351)).await;
            42
        },
        || {
            let count = renewals.get();
            assert_eq!(start.elapsed(), Duration::from_millis(count * 50));
            renewals.set(count + 1);
            std::future::ready(true)
        },
    )
    .await;
    assert!(matches!(result, Outcome::Completed(42)));
    assert_eq!(renewals.get(), 8);
}

#[tokio::test(start_paused = true)]
async fn failed_initial_renewal_does_not_poll_ready_work() {
    let result = renewing(150, async { panic!("unauthorized work") }, || {
        std::future::ready(false)
    })
    .await;
    assert!(matches!(result, Outcome::LeaseLost));
}

struct Dropped<'a>(&'a Cell<bool>);
impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

#[tokio::test(start_paused = true)]
async fn failed_later_renewal_drops_inflight_work() {
    let dropped = Cell::new(false);
    let polled = Cell::new(false);
    let calls = Cell::new(0);
    let guard = Dropped(&dropped);
    let result = renewing(
        150,
        async {
            let _guard = guard;
            polled.set(true);
            tokio::time::sleep(Duration::from_millis(100)).await;
            panic!("side effect after renewal failure");
        },
        || {
            calls.set(calls.get() + 1);
            std::future::ready(calls.get() == 1)
        },
    )
    .await;
    assert!(matches!(result, Outcome::LeaseLost));
    assert_eq!(calls.get(), 2);
    assert!(polled.get());
    assert!(dropped.get());
}

#[tokio::test(start_paused = true)]
async fn blocked_renewal_times_out_and_drops_both_futures() {
    let work_dropped = Cell::new(false);
    let renewal_dropped = Cell::new(false);
    let work_guard = Dropped(&work_dropped);
    let start = tokio::time::Instant::now();
    let run = renewing(
        150,
        async {
            let _guard = work_guard;
            panic!("work polled before renewal");
        },
        || async {
            let _guard = Dropped(&renewal_dropped);
            std::future::pending::<bool>().await
        },
    );
    let result = tokio::time::timeout(Duration::from_millis(500), run)
        .await
        .expect("renewal timeout must beat outer watchdog");
    assert!(matches!(result, Outcome::LeaseLost));
    assert!(start.elapsed() >= Duration::from_millis(50));
    assert!(start.elapsed() < Duration::from_millis(100));
    assert!(work_dropped.get());
    assert!(renewal_dropped.get());
}
