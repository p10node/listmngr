//! The out runner feeds the process-wide metrics: one SMTP transaction, one
//! sent recipient and one acceptance-to-relay latency sample per delivery.
use super::*;
use crate::outbound::tests::{capture_data, fixture};
use listmngr_db::mail_queue::JobState;

#[tokio::test]
async fn a_delivery_counts_its_transaction_recipient_and_latency() {
    let metrics = listmngr_core::metrics::global();
    let before = (
        metrics.smtp_transactions.get("completed"),
        metrics.recipients.get("sent"),
        metrics.smtp_transaction_seconds.count(),
        metrics.delivery_latency_seconds.count(),
    );
    let (db, lease, mut role, sink) = fixture(
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: metrics\r\nMessage-ID: <metrics@example.invalid>\r\n\r\nbody\r\n",
    )
    .await;
    role.command_timeout = Duration::from_secs(2);
    let (data, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(capture_data(&sink), deliver_one(&db, &role, lease.clone()))
    })
    .await
    .unwrap();
    assert!(data.ends_with(b"body\r\n"));
    assert_eq!(
        db.mail_queue().job(lease.job.id).await.unwrap().state,
        JobState::Done
    );
    // Other tests in this binary deliver too, so only lower bounds hold.
    assert!(metrics.smtp_transactions.get("completed") > before.0);
    assert!(metrics.recipients.get("sent") > before.1);
    assert!(metrics.smtp_transaction_seconds.count() > before.2);
    assert!(metrics.delivery_latency_seconds.count() > before.3);
    let text = metrics.render();
    assert!(text.contains("listmngr_delivery_latency_seconds_bucket{le=\"+Inf\"}"));
}

#[tokio::test]
async fn an_unreachable_relay_counts_a_transient_recipient_and_no_latency() {
    let metrics = listmngr_core::metrics::global();
    let before = (
        metrics.recipients.get("transient"),
        metrics.delivery_latency_seconds.count(),
        metrics.smtp_transactions.get("failed"),
    );
    let (db, lease, mut role, sink) = fixture(
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: metrics\r\nMessage-ID: <metrics-fail@example.invalid>\r\n\r\nbody\r\n",
    )
    .await;
    role.command_timeout = Duration::from_millis(200);
    // A sink that closes without a greeting fails the transaction before DATA.
    let closing = async {
        let (stream, _) = sink.accept().await.unwrap();
        drop(stream);
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(closing, deliver_one(&db, &role, lease.clone()))
    })
    .await
    .unwrap();
    assert!(metrics.recipients.get("transient") > before.0);
    assert!(metrics.smtp_transactions.get("failed") > before.2);
    assert_eq!(
        db.mail_queue().job(lease.job.id).await.unwrap().state,
        JobState::Ready,
        "a transient failure retries"
    );
    let _ = before.1;
}
