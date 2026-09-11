use super::*;
use tokio::{
    sync::{mpsc, oneshot},
    time::{advance, timeout},
};

type PageReply = oneshot::Sender<listmngr_core::Result<BounceSweepSummary>>;

async fn next(
    calls: &mut mpsc::UnboundedReceiver<(Option<Uuid>, PageReply)>,
) -> (Option<Uuid>, PageReply) {
    timeout(Duration::from_secs(1), calls.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn cursor_empty_reset_failure_retry_and_completion_delay() {
    let interval = Duration::from_secs(60);
    let (tx, rx) = watch::channel(false);
    let (report, mut calls) = mpsc::unbounded_channel();
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(schedule(interval, rx, move |cursor| {
        let (reply, received) = oneshot::channel();
        report.send((cursor, reply)).unwrap();
        async move { received.await.unwrap() }
    }));
    tokio::task::yield_now().await;
    assert!(calls.try_recv().is_err());
    advance(interval - Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert!(calls.try_recv().is_err());
    advance(Duration::from_secs(1)).await;
    let (cursor, reply) = next(&mut calls).await;
    assert_eq!(cursor, None);
    advance(Duration::from_secs(600)).await;
    tokio::task::yield_now().await;
    assert!(calls.try_recv().is_err(), "overlapping page");
    let id = Uuid::now_v7();
    reply
        .send(Ok(BounceSweepSummary {
            scanned: 2,
            failed: 1,
            next_cursor: Some(id),
            ..Default::default()
        }))
        .unwrap();
    tokio::task::yield_now().await;
    assert!(calls.try_recv().is_err(), "catch-up burst");
    advance(interval - Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert!(calls.try_recv().is_err());
    advance(Duration::from_secs(1)).await;
    let (cursor, reply) = next(&mut calls).await;
    assert_eq!(cursor, Some(id), "non-due/failed page did not advance");
    reply
        .send(Err(listmngr_core::Error::Validation(
            "private error must not be logged".into(),
        )))
        .unwrap();
    tokio::task::yield_now().await;
    assert!(calls.try_recv().is_err(), "busy retry");
    advance(interval).await;
    let (cursor, reply) = next(&mut calls).await;
    assert_eq!(cursor, Some(id), "page error must retain cursor");
    reply.send(Ok(BounceSweepSummary::default())).unwrap();
    tokio::task::yield_now().await;
    assert!(
        calls.try_recv().is_err(),
        "empty page must wait before wrap"
    );
    advance(interval).await;
    let (cursor, reply) = next(&mut calls).await;
    assert_eq!(cursor, None);
    reply.send(Ok(BounceSweepSummary::default())).unwrap();
    tx.send(true).unwrap();
    timeout(Duration::from_secs(1), tasks.join_next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn shutdown_cancels_blocked_page_and_handles_preclosed_watch() {
    for closed in [false, true] {
        let (tx, rx) = watch::channel(false);
        let (report, mut calls) = mpsc::unbounded_channel();
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(schedule(Duration::from_secs(60), rx, move |_| {
            let (owned, receipt) = oneshot::channel::<()>();
            report.send(receipt).unwrap();
            async move {
                let _owned = owned;
                std::future::pending().await
            }
        }));
        let receipt = timeout(Duration::from_secs(61), calls.recv())
            .await
            .unwrap()
            .unwrap();
        if closed {
            drop(tx);
        } else {
            tx.send(true).unwrap();
        }
        timeout(Duration::from_secs(1), tasks.join_next())
            .await
            .expect("blocked page survived shutdown")
            .unwrap()
            .unwrap();
        assert!(receipt.await.is_err(), "page future detached");
    }
    for closed in [false, true] {
        let (tx, rx) = watch::channel(!closed);
        if closed {
            drop(tx);
        }
        timeout(
            Duration::from_secs(1),
            schedule(Duration::from_secs(60), rx, |_| async {
                panic!("page ran after shutdown")
            }),
        )
        .await
        .unwrap();
    }
}
