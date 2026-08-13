//! Reconnect delay computation, mirroring cloudflared's
//! `retry.BackoffHandler`: a random value in `[0, base * 2^retries)`.

use std::time::Duration;

/// The reconnect delay for a failed attempt.
pub(crate) fn retry_delay(retries: u32, base: Duration) -> Duration {
    let exponent = retries.min(30);
    let maximum_nanos = (base.as_nanos() as u64)
        .saturating_mul(1u64 << exponent)
        .min(1u64 << 62);
    if maximum_nanos == 0 {
        return Duration::ZERO;
    }
    let mut buffer = [0u8; 8];
    let _ = getrandom::fill(&mut buffer);
    let nanos = u64::from_le_bytes(buffer) % maximum_nanos;
    Duration::from_nanos(nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_bounded_by_base_times_two_pow() {
        let base = Duration::from_secs(1);
        for retries in 0..10 {
            let delay = retry_delay(retries, base);
            let maximum = Duration::from_secs(1).saturating_mul(1 << retries.min(30));
            assert!(
                delay <= maximum,
                "retries={retries} delay={delay:?} maximum={maximum:?}"
            );
        }
    }

    #[test]
    fn zero_base_backoff_is_instant() {
        assert_eq!(retry_delay(5, Duration::ZERO), Duration::ZERO);
    }
}
