//! In-memory latency histogram for detection pipeline instrumentation.
//! Records time from WS receive to spread computation in the detector.

use std::sync::Mutex;
use std::time::Duration;

/// Shared latency stats. Detector records, API reads.
/// Values stored in microseconds.
pub struct LatencyStats {
    inner: Mutex<hdrhistogram::Histogram<u64>>,
}

impl LatencyStats {
    /// Create a new histogram. Tracks 1us to 100s, 3 significant figures.
    pub fn new() -> Self {
        let histogram = hdrhistogram::Histogram::new_with_bounds(1, 100_000_000, 3)
            .expect("valid histogram bounds");
        Self {
            inner: Mutex::new(histogram),
        }
    }

    /// Record a detection latency in microseconds.
    pub fn record_us(&self, us: u64) {
        if let Ok(mut h) = self.inner.lock() {
            let _ = h.record(us);
        }
    }

    /// Record from a std::time::Duration.
    pub fn record(&self, d: Duration) {
        let us = d.as_micros().min(u128::from(u64::MAX)) as u64;
        self.record_us(us);
    }

    /// Return (p50_us, p95_us, p99_us). None if no samples.
    pub fn percentiles(&self) -> (Option<u64>, Option<u64>, Option<u64>) {
        let Ok(h) = self.inner.lock() else {
            return (None, None, None);
        };
        if h.len() == 0 {
            return (None, None, None);
        }
        let p50 = h.value_at_quantile(0.5);
        let p95 = h.value_at_quantile(0.95);
        let p99 = h.value_at_quantile(0.99);
        (Some(p50), Some(p95), Some(p99))
    }

    /// Sample count.
    pub fn len(&self) -> u64 {
        self.inner.lock().map(|h| h.len()).unwrap_or(0)
    }
}

impl Default for LatencyStats {
    fn default() -> Self {
        Self::new()
    }
}
