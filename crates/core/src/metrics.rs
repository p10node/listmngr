//! Process-wide mail metrics for `/metrics`.
//!
//! The mail role runs in the same process as the HTTP server, so counters
//! and histograms are plain atomics behind one global registry; queue depth
//! comes from the database at scrape time and is rendered by the API. The
//! exposition is written by hand in the Prometheus text format (no
//! registry crate): every series is declared up front with its label
//! values, so a scrape shows zeros rather than missing series.
use std::fmt::Write as _;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// A counter split by one label with a fixed set of values.
#[derive(Debug)]
pub struct LabeledCounter<const N: usize> {
    name: &'static str,
    help: &'static str,
    label: &'static str,
    values: [&'static str; N],
    counters: [AtomicU64; N],
}

impl<const N: usize> LabeledCounter<N> {
    #[must_use]
    pub const fn new(
        name: &'static str,
        help: &'static str,
        label: &'static str,
        values: [&'static str; N],
    ) -> Self {
        Self {
            name,
            help,
            label,
            values,
            counters: [const { AtomicU64::new(0) }; N],
        }
    }

    fn index(&self, value: &str) -> Option<usize> {
        self.values.iter().position(|known| *known == value)
    }

    /// Add one to `value`'s series; an undeclared value is ignored.
    pub fn inc(&self, value: &str) {
        self.add(value, 1);
    }

    /// Add `by` to `value`'s series; an undeclared value is ignored.
    pub fn add(&self, value: &str, by: u64) {
        if let Some(index) = self.index(value) {
            self.counters[index].fetch_add(by, Ordering::Relaxed);
        }
    }

    #[must_use]
    pub fn get(&self, value: &str) -> u64 {
        self.index(value)
            .map_or(0, |index| self.counters[index].load(Ordering::Relaxed))
    }

    pub fn render(&self, out: &mut String) {
        let _ = writeln!(out, "# HELP {} {}", self.name, self.help);
        let _ = writeln!(out, "# TYPE {} counter", self.name);
        for (value, counter) in self.values.iter().zip(&self.counters) {
            let _ = writeln!(
                out,
                "{}{{{}=\"{}\"}} {}",
                self.name,
                self.label,
                value,
                counter.load(Ordering::Relaxed)
            );
        }
    }
}

/// A histogram of seconds with fixed upper bounds (plus `+Inf`).
#[derive(Debug)]
pub struct Histogram<const N: usize> {
    name: &'static str,
    help: &'static str,
    bounds: [f64; N],
    buckets: [AtomicU64; N],
    sum_micros: AtomicU64,
    count: AtomicU64,
}

impl<const N: usize> Histogram<N> {
    #[must_use]
    pub const fn new(name: &'static str, help: &'static str, bounds: [f64; N]) -> Self {
        Self {
            name,
            help,
            bounds,
            buckets: [const { AtomicU64::new(0) }; N],
            sum_micros: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    /// Record one observation; negative values count as zero.
    pub fn observe(&self, seconds: f64) {
        let seconds = if seconds.is_finite() {
            seconds.max(0.0)
        } else {
            0.0
        };
        for (bound, bucket) in self.bounds.iter().zip(&self.buckets) {
            if seconds <= *bound {
                bucket.fetch_add(1, Ordering::Relaxed);
            }
        }
        // Whole microseconds keep the sum exact under concurrent adds.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let micros = (seconds * 1_000_000.0).round() as u64;
        self.sum_micros.fetch_add(micros, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    pub fn render(&self, out: &mut String) {
        let _ = writeln!(out, "# HELP {} {}", self.name, self.help);
        let _ = writeln!(out, "# TYPE {} histogram", self.name);
        for (bound, bucket) in self.bounds.iter().zip(&self.buckets) {
            let _ = writeln!(
                out,
                "{}_bucket{{le=\"{}\"}} {}",
                self.name,
                bound,
                bucket.load(Ordering::Relaxed)
            );
        }
        let count = self.count.load(Ordering::Relaxed);
        let _ = writeln!(out, "{}_bucket{{le=\"+Inf\"}} {count}", self.name);
        #[allow(clippy::cast_precision_loss)]
        let sum = self.sum_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        let _ = writeln!(out, "{}_sum {sum}", self.name);
        let _ = writeln!(out, "{}_count {count}", self.name);
    }
}

/// Every in-process mail metric.
#[derive(Debug)]
pub struct Metrics {
    /// LMTP `RCPT` outcomes as the front MTA saw them.
    pub lmtp_recipients: LabeledCounter<3>,
    /// What the `in` runner decided for each queued submission.
    pub posts: LabeledCounter<8>,
    /// Per-recipient outcomes of outgoing SMTP transactions.
    pub recipients: LabeledCounter<4>,
    /// Outgoing SMTP transactions to the relay.
    pub smtp_transactions: LabeledCounter<2>,
    pub smtp_transaction_seconds: Histogram<12>,
    /// Acceptance at LMTP to the relay accepting a recipient.
    pub delivery_latency_seconds: Histogram<12>,
    /// Bounce reports by what the runner made of them; `scored` counts
    /// members, the others count reports.
    pub bounces: LabeledCounter<4>,
    /// Webhook deliveries by what one attempt made of them.
    pub webhook_deliveries: LabeledCounter<3>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            lmtp_recipients: LabeledCounter::new(
                "listmngr_lmtp_recipients_total",
                "LMTP recipients by outcome: accepted (250), rejected (550) or deferred (451).",
                "result",
                ["accepted", "rejected", "deferred"],
            ),
            posts: LabeledCounter::new(
                "listmngr_posts_total",
                "Inbound submissions by the in runner's disposition.",
                "disposition",
                [
                    "accepted",
                    "held",
                    "rejected",
                    "discarded",
                    "filtered",
                    "owner",
                    "command",
                    "failed",
                ],
            ),
            recipients: LabeledCounter::new(
                "listmngr_delivery_recipients_total",
                "Outgoing recipients by SMTP outcome.",
                "result",
                ["sent", "transient", "permanent", "ambiguous"],
            ),
            smtp_transactions: LabeledCounter::new(
                "listmngr_smtp_transactions_total",
                "Outgoing SMTP transactions: completed (DATA answered) or failed before DATA.",
                "result",
                ["completed", "failed"],
            ),
            smtp_transaction_seconds: Histogram::new(
                "listmngr_smtp_transaction_seconds",
                "Duration of one outgoing SMTP transaction.",
                [
                    0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
                ],
            ),
            delivery_latency_seconds: Histogram::new(
                "listmngr_delivery_latency_seconds",
                "Seconds from LMTP acceptance to the relay accepting a recipient.",
                [
                    0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0, 900.0, 3600.0,
                ],
            ),
            bounces: LabeledCounter::new(
                "listmngr_bounces_total",
                "Bounce reports: recognized, unrecognized or failed; scored counts members.",
                "result",
                ["recognized", "unrecognized", "failed", "scored"],
            ),
            webhook_deliveries: LabeledCounter::new(
                "listmngr_webhook_deliveries_total",
                "Webhook delivery attempts: delivered, retried or failed.",
                "result",
                ["delivered", "retried", "failed"],
            ),
        }
    }

    /// The Prometheus exposition of every metric.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.lmtp_recipients.render(&mut out);
        self.posts.render(&mut out);
        self.recipients.render(&mut out);
        self.smtp_transactions.render(&mut out);
        self.smtp_transaction_seconds.render(&mut out);
        self.delivery_latency_seconds.render(&mut out);
        self.bounces.render(&mut out);
        self.webhook_deliveries.render(&mut out);
        out
    }
}

/// The process-wide registry.
pub fn global() -> &'static Metrics {
    static METRICS: OnceLock<Metrics> = OnceLock::new();
    METRICS.get_or_init(Metrics::new)
}
