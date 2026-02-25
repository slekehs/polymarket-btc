//! Shared health state for the /health endpoint.
//! Updated by WsManager, window_consumer, and DbWriter.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Shared health metrics. Updated by scanner components, read by API.
#[derive(Default)]
pub struct HealthState {
    /// True when WS is connected and in the main loop.
    pub ws_connected: AtomicBool,
    /// Nanosecond timestamp of last window close event (0 = none).
    pub last_window_at_ns: AtomicU64,
    /// Approximate count of window close events queued for DB write.
    pub write_queue_pending: AtomicU64,
}

impl HealthState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_ws_connected(&self, v: bool) {
        self.ws_connected.store(v, Ordering::Relaxed);
    }

    pub fn set_last_window_at_ns(&self, ns: u64) {
        self.last_window_at_ns.store(ns, Ordering::Relaxed);
    }

    pub fn inc_write_queue_pending(&self) {
        self.write_queue_pending.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_write_queue_pending(&self) {
        self.write_queue_pending.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn ws_connected(&self) -> bool {
        self.ws_connected.load(Ordering::Relaxed)
    }

    pub fn last_window_at_ns(&self) -> u64 {
        self.last_window_at_ns.load(Ordering::Relaxed)
    }

    pub fn write_queue_pending(&self) -> u64 {
        self.write_queue_pending.load(Ordering::Relaxed)
    }
}
