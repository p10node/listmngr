//! Process-wide mail metrics: labeled counters and fixed-bucket histograms
//! rendered as Prometheus exposition text.
use listmngr_core::metrics::{Histogram, LabeledCounter, Metrics};

#[test]
fn a_labeled_counter_renders_one_series_per_known_value() {
    let counter: LabeledCounter<3> = LabeledCounter::new(
        "listmngr_things_total",
        "Things.",
        "result",
        ["ok", "failed", "skipped"],
    );
    counter.inc("ok");
    counter.inc("ok");
    counter.add("failed", 5);
    counter.inc("unknown"); // ignored: only declared values exist
    assert_eq!(counter.get("ok"), 2);
    assert_eq!(counter.get("failed"), 5);
    assert_eq!(counter.get("skipped"), 0);
    let mut out = String::new();
    counter.render(&mut out);
    assert_eq!(
        out,
        "# HELP listmngr_things_total Things.\n# TYPE listmngr_things_total counter\nlistmngr_things_total{result=\"ok\"} 2\nlistmngr_things_total{result=\"failed\"} 5\nlistmngr_things_total{result=\"skipped\"} 0\n"
    );
}

#[test]
fn a_histogram_accumulates_cumulative_buckets_sum_and_count() {
    let histogram: Histogram<3> =
        Histogram::new("listmngr_wait_seconds", "Wait.", [0.1, 1.0, 10.0]);
    histogram.observe(0.05);
    histogram.observe(0.5);
    histogram.observe(1.0);
    histogram.observe(42.0);
    histogram.observe(-1.0); // clamped to zero, still counted
    assert_eq!(histogram.count(), 5);
    let mut out = String::new();
    histogram.render(&mut out);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "# HELP listmngr_wait_seconds Wait.");
    assert_eq!(lines[1], "# TYPE listmngr_wait_seconds histogram");
    assert_eq!(lines[2], "listmngr_wait_seconds_bucket{le=\"0.1\"} 2");
    assert_eq!(lines[3], "listmngr_wait_seconds_bucket{le=\"1\"} 4");
    assert_eq!(lines[4], "listmngr_wait_seconds_bucket{le=\"10\"} 4");
    assert_eq!(lines[5], "listmngr_wait_seconds_bucket{le=\"+Inf\"} 5");
    assert_eq!(lines[6], "listmngr_wait_seconds_sum 43.55");
    assert_eq!(lines[7], "listmngr_wait_seconds_count 5");
}

#[test]
fn the_global_registry_renders_every_mail_metric() {
    let metrics = Metrics::new();
    metrics.lmtp_recipients.inc("accepted");
    metrics.posts.inc("held");
    metrics.recipients.add("sent", 3);
    metrics.smtp_transactions.inc("completed");
    metrics.smtp_transaction_seconds.observe(0.2);
    metrics.delivery_latency_seconds.observe(3.0);
    let text = metrics.render();
    for needle in [
        "# TYPE listmngr_lmtp_recipients_total counter",
        "listmngr_lmtp_recipients_total{result=\"accepted\"} 1",
        "listmngr_lmtp_recipients_total{result=\"rejected\"} 0",
        "listmngr_lmtp_recipients_total{result=\"deferred\"} 0",
        "listmngr_posts_total{disposition=\"held\"} 1",
        "listmngr_posts_total{disposition=\"accepted\"} 0",
        "listmngr_posts_total{disposition=\"rejected\"} 0",
        "listmngr_posts_total{disposition=\"discarded\"} 0",
        "listmngr_posts_total{disposition=\"filtered\"} 0",
        "listmngr_posts_total{disposition=\"owner\"} 0",
        "listmngr_posts_total{disposition=\"command\"} 0",
        "listmngr_posts_total{disposition=\"failed\"} 0",
        "listmngr_delivery_recipients_total{result=\"sent\"} 3",
        "listmngr_delivery_recipients_total{result=\"transient\"} 0",
        "listmngr_delivery_recipients_total{result=\"permanent\"} 0",
        "listmngr_delivery_recipients_total{result=\"ambiguous\"} 0",
        "listmngr_smtp_transactions_total{result=\"completed\"} 1",
        "listmngr_smtp_transactions_total{result=\"failed\"} 0",
        "# TYPE listmngr_smtp_transaction_seconds histogram",
        "listmngr_smtp_transaction_seconds_bucket{le=\"0.25\"} 1",
        "listmngr_smtp_transaction_seconds_count 1",
        "# TYPE listmngr_delivery_latency_seconds histogram",
        "listmngr_delivery_latency_seconds_bucket{le=\"2.5\"} 0",
        "listmngr_delivery_latency_seconds_bucket{le=\"5\"} 1",
        "listmngr_delivery_latency_seconds_count 1",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
    }
    // The process-wide instance is the same object every time.
    assert!(std::ptr::eq(
        listmngr_core::metrics::global(),
        listmngr_core::metrics::global()
    ));
}
