//! Endpoint health tracking and ordered selection for chunk fetches.
//!
//! Measured on this network (2026-11-18): probing every CDN host for every
//! chunk produced 44-81 simultaneous connections for a single stream, many of
//! them stuck in TCP SYN_SENT (the host never answered). The healthy host was
//! buried inside that race, so a chunk could sit for the full 60s timeout and a
//! 1 MiB read stalled playback for minutes.
//!
//! This pool keeps a per-endpoint health record and hands out a short ordered
//! candidate list per chunk instead: usually one host to try, a second as a
//! fallback, and cooling-down hosts skipped until their cooldown expires.

use std::time::{Duration, Instant};

/// How many hosts one fetch_chunk attempt may try before failing.
pub const MAX_ATTEMPTS: usize = 2;
/// A host that just failed is skipped for this long.
pub const FAILURE_COOLDOWN: Duration = Duration::from_secs(30);
/// Exponential moving average weight for latency.
const LATENCY_ALPHA: f64 = 0.3;

#[derive(Debug, Clone, Default)]
struct Health {
    /// Consecutive failures; reset on success.
    failures: u32,
    last_failure: Option<Instant>,
    latency_ms: Option<f64>,
    last_success: Option<Instant>,
}

/// Health state for one stream's endpoint list.
pub struct EndpointPool {
    health: Vec<Health>,
}

impl EndpointPool {
    pub fn new(len: usize) -> Self {
        Self {
            health: vec![Health::default(); len],
        }
    }

    /// Record a successful chunk fetch and its wall-clock time.
    pub fn mark_success(&mut self, idx: usize, elapsed: Duration) {
        if let Some(h) = self.health.get_mut(idx) {
            h.failures = 0;
            h.last_failure = None;
            h.last_success = Some(Instant::now());
            let ms = elapsed.as_secs_f64() * 1000.0;
            h.latency_ms = Some(match h.latency_ms {
                Some(old) => old * (1.0 - LATENCY_ALPHA) + ms * LATENCY_ALPHA,
                None => ms,
            });
        }
    }

    /// Record a failed attempt (timeout, connection error or HTTP error).
    pub fn mark_failure(&mut self, idx: usize) {
        if let Some(h) = self.health.get_mut(idx) {
            h.failures = h.failures.saturating_add(1);
            h.last_failure = Some(Instant::now());
        }
    }

    /// Whether the endpoint is inside its failure cooldown.
    pub fn is_cooling(&self, idx: usize, now: Instant) -> bool {
        self.health
            .get(idx)
            .and_then(|h| h.last_failure)
            .map(|at| now.duration_since(at) < FAILURE_COOLDOWN)
            .unwrap_or(false)
    }

    /// Ordered candidates for one chunk.
    ///
    /// Ranking: not-cooling first; then fewer consecutive failures; then lower
    /// average latency. Hosts with no health data sort before failed ones but
    /// after proven-good ones. At most MAX_ATTEMPTS indices are returned so one
    /// chunk can never fan out across the whole endpoint list again.
    pub fn candidates(&self, now: Instant) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.health.len()).collect();
        order.sort_by_key(|&idx| {
            let h = &self.health[idx];
            let cooling = self.is_cooling(idx, now);
            let latency = h.latency_ms.unwrap_or(f64::MAX) as u64;
            (cooling, h.failures, latency)
        });
        order.truncate(MAX_ATTEMPTS);
        order
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_successful_host_is_preferred_over_unknown_ones() {
        let mut pool = EndpointPool::new(3);
        pool.mark_success(2, Duration::from_millis(120));
        let candidates = pool.candidates(Instant::now());
        assert_eq!(candidates.first(), Some(&2));
        assert!(candidates.len() <= MAX_ATTEMPTS);
    }

    #[test]
    fn a_failed_host_moves_behind_healthy_ones() {
        let mut pool = EndpointPool::new(3);
        pool.mark_failure(0);
        let candidates = pool.candidates(Instant::now());
        assert!(
            !candidates.contains(&0),
            "cooling host must not be tried first"
        );
        assert_eq!(candidates.len(), MAX_ATTEMPTS);
    }

    #[test]
    fn a_host_recovers_after_the_cooldown() {
        let mut pool = EndpointPool::new(2);
        pool.mark_failure(0);
        let now = Instant::now();
        assert!(pool.is_cooling(0, now));
        assert!(!pool.is_cooling(0, now + FAILURE_COOLDOWN + Duration::from_secs(1)));
    }

    #[test]
    fn all_cooling_still_returns_bounded_candidates() {
        let mut pool = EndpointPool::new(4);
        for idx in 0..4 {
            pool.mark_failure(idx);
        }
        let candidates = pool.candidates(Instant::now());
        assert_eq!(candidates.len(), MAX_ATTEMPTS);
    }

    #[test]
    fn fewer_failures_outrank_more_failures_when_both_are_cooling() {
        let mut pool = EndpointPool::new(2);
        pool.mark_failure(0);
        pool.mark_failure(1);
        pool.mark_failure(1);
        let candidates = pool.candidates(Instant::now());
        assert_eq!(candidates.first(), Some(&0));
        assert_eq!(candidates.len(), 2);
    }
}
