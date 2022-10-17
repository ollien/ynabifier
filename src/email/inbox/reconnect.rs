use std::time::Duration;

use tokio::time;

const BASE_DELAY_SECS: u64 = 1;

/// Provides a basic delay mechanism for reconnections
pub struct Delay {
    delay: Duration,
}

impl Default for Delay {
    fn default() -> Self {
        Self {
            delay: Duration::from_secs(BASE_DELAY_SECS),
        }
    }
}

impl Delay {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get how long the next call to [`backoff_wait`] will wait.
    pub fn peek_delay(&self) -> Duration {
        self.delay
    }

    /// Wait for the current delay number of seconds, and then backoff for the next delay.
    pub async fn backoff_wait(&mut self) {
        time::sleep(self.delay).await;
        self.backoff();
    }

    fn backoff(&mut self) {
        self.delay *= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backoff_doubles_wait() {
        let mut delay = Delay::new();
        assert_eq!(Duration::from_secs(1), delay.peek_delay());

        delay.backoff();
        assert_eq!(Duration::from_secs(2), delay.peek_delay());

        delay.backoff();
        assert_eq!(Duration::from_secs(4), delay.peek_delay());

        delay.backoff();
        assert_eq!(Duration::from_secs(8), delay.peek_delay());
    }
}
