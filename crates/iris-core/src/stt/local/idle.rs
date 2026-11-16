/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! When an idle on-device model is unloaded (Android `OnDeviceRecognizer.idleReleaseWaitMs`, the Unload Model setting).

/// Milliseconds until an idle model is released: None = never (`unload_minutes` < 0), ≤ 0 = now. Idle time counts
/// from the later of `idle_since` and `last_use` (the last load or decode); all on one millisecond clock.
pub fn idle_release_wait_ms(unload_minutes: i32, idle_since: i64, last_use: i64, now: i64) -> Option<i64> {
    (unload_minutes >= 0).then(|| unload_minutes as i64 * 60_000 - (now - idle_since.max(last_use)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;
    const NOW: i64 = 100 * MIN;

    #[test]
    fn never_releases() {
        assert_eq!(None, idle_release_wait_ms(-1, 0, 0, NOW));
    }

    #[test]
    fn immediately_releases_now() {
        assert_eq!(Some(0), idle_release_wait_ms(0, NOW, NOW - MIN, NOW));
    }

    #[test]
    fn counts_from_when_it_became_idle() {
        assert_eq!(Some(4 * MIN), idle_release_wait_ms(5, NOW - MIN, NOW - 10 * MIN, NOW));
    }

    #[test]
    fn a_later_use_counts_from_that_use() {
        assert_eq!(Some(4 * MIN), idle_release_wait_ms(5, NOW - 4 * MIN, NOW - MIN, NOW));
    }

    #[test]
    fn a_shorter_delay_than_the_idle_time_releases_now() {
        assert_eq!(Some(-2 * MIN), idle_release_wait_ms(2, NOW - 4 * MIN, 0, NOW));
    }
}
