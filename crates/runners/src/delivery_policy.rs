//! Delivery policy shared by the runners: how transient failures back off
//! and how a recipient roster is cut into SMTP transactions.
//!
//! Backoff is exponential in the job's attempt count with a cap and a
//! random jitter, so a relay outage does not produce a thundering herd of
//! retries; chunking groups recipients by domain the way Mailman's
//! `chunkify` does, so one transaction reaches one remote server as far as
//! possible.
use rand::Rng;

/// `[mta] retry_initial_secs` / `retry_max_secs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub initial_ms: i64,
    pub max_ms: i64,
}

impl Backoff {
    /// The delay before attempt `attempts + 1`, doubling from `initial_ms`
    /// per attempt already made, capped at `max_ms`, then jittered by up to
    /// ±20% (never below one second).
    #[must_use]
    pub fn delay_ms(self, attempts: i64) -> i64 {
        let exponent = u32::try_from(attempts.saturating_sub(1).clamp(0, 30)).unwrap_or(30);
        let base = self
            .initial_ms
            .saturating_mul(1_i64 << exponent)
            .min(self.max_ms)
            .max(1_000);
        let jitter = base / 5;
        let spread = rand::rng().random_range(-jitter..=jitter);
        base.saturating_add(spread).max(1_000)
    }
}

/// Cut `recipients` (kept by index) into transactions of at most `size`,
/// with recipients of the same domain adjacent. Every index appears once.
#[must_use]
pub fn chunk_by_domain(recipients: &[String], size: usize) -> Vec<Vec<usize>> {
    let size = size.max(1);
    let mut order: Vec<usize> = (0..recipients.len()).collect();
    let domain = |index: usize| {
        recipients[index]
            .rsplit_once('@')
            .map_or("", |(_, domain)| domain)
            .to_ascii_lowercase()
    };
    // Stable: recipients of one domain keep their roster order.
    order.sort_by_cached_key(|index| domain(*index));
    order.chunks(size).map(<[usize]>::to_vec).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_caps_and_jitters_within_bounds() {
        let policy = Backoff {
            initial_ms: 10_000,
            max_ms: 3_600_000,
        };
        for (attempts, expected) in [
            (0, 10_000),
            (1, 10_000),
            (2, 20_000),
            (3, 40_000),
            (10, 3_600_000),
            (100, 3_600_000),
        ] {
            for _ in 0..20 {
                let delay = policy.delay_ms(attempts);
                let low = expected - expected / 5;
                let high = expected + expected / 5;
                assert!(
                    (low..=high).contains(&delay),
                    "attempts={attempts}: {delay} not within {low}..={high}"
                );
            }
        }
        assert!(
            Backoff {
                initial_ms: 100,
                max_ms: 200
            }
            .delay_ms(1)
                >= 1_000,
            "never below a second"
        );
    }

    #[test]
    fn chunks_group_by_domain_and_cover_every_recipient_once() {
        let recipients: Vec<String> = [
            "a@one.invalid",
            "b@two.invalid",
            "c@one.invalid",
            "d@Three.invalid",
            "e@two.invalid",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let chunks = chunk_by_domain(&recipients, 2);
        assert_eq!(chunks, vec![vec![0, 2], vec![3, 1], vec![4]]);
        let mut seen: Vec<usize> = chunks.iter().flatten().copied().collect();
        seen.sort_unstable();
        assert_eq!(seen, [0, 1, 2, 3, 4]);
        assert_eq!(chunk_by_domain(&recipients, 1).len(), 5);
        assert_eq!(chunk_by_domain(&recipients, 500).len(), 1);
        assert!(chunk_by_domain(&[], 5).is_empty());
        assert_eq!(
            chunk_by_domain(&recipients, 0).len(),
            5,
            "a zero size still delivers"
        );
    }
}
