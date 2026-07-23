use std::time::Duration;

/// Retry policy for [`super::HerdrEvents::connect_with_retry`].
#[derive(Debug, Clone)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
    pub multiplier: f64,
    /// `None` = retry forever (daemon default); `Some(n)` = give up after `n`
    /// failed attempts and return the last error.
    pub max_retries: Option<usize>,
}

impl Default for Backoff {
    fn default() -> Backoff {
        Backoff {
            initial: Duration::from_millis(200),
            max: Duration::from_secs(5),
            multiplier: 2.0,
            max_retries: None,
        }
    }
}

impl Backoff {
    /// A bounded policy (useful for tests).
    pub fn bounded(max_retries: usize) -> Backoff {
        Backoff {
            max_retries: Some(max_retries),
            ..Backoff::default()
        }
    }

    pub(super) fn next_delay(&self, current: Duration) -> Duration {
        let next = current.mul_f64(self.multiplier);
        if next > self.max {
            self.max
        } else {
            next
        }
    }
}
