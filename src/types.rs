use serde::{Deserialize, Serialize};
use std::time::Instant;

// ---------------------------------------------------------------------------
// Market
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    pub id: String,
    pub question: String,
    pub category: Category,
    pub end_date_iso: Option<String>,
    pub total_volume: Option<f64>,
    pub yes_token_id: String,
    pub no_token_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Sports,
    Weather,
    Crypto,
    Politics,
    Economics,
    Other,
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Category::Sports => "sports",
            Category::Weather => "weather",
            Category::Crypto => "crypto",
            Category::Politics => "politics",
            Category::Economics => "economics",
            Category::Other => "other",
        };
        write!(f, "{s}")
    }
}

// ---------------------------------------------------------------------------
// Spread classification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpreadCategory {
    /// spread < $0.02 — below viable threshold
    Noise,
    /// spread $0.02–$0.05
    Small,
    /// spread $0.05–$0.10
    Medium,
    /// spread > $0.10
    Large,
}

impl SpreadCategory {
    pub fn from_spread(spread: f64) -> Self {
        use crate::config::spread_thresholds::*;
        if spread < NOISE_MAX {
            SpreadCategory::Noise
        } else if spread < SMALL_MAX {
            SpreadCategory::Small
        } else if spread < MEDIUM_MAX {
            SpreadCategory::Medium
        } else {
            SpreadCategory::Large
        }
    }
}

impl std::fmt::Display for SpreadCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SpreadCategory::Noise => "noise",
            SpreadCategory::Small => "small",
            SpreadCategory::Medium => "medium",
            SpreadCategory::Large => "large",
        };
        write!(f, "{s}")
    }
}

// ---------------------------------------------------------------------------
// Window classification
// ---------------------------------------------------------------------------

/// Dimension 1 — was this a real order?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenDurationClass {
    /// Opened and closed within 1 tick — stale order noise.
    SingleTick,
    /// Survived 2+ consecutive ticks — real opportunity.
    MultiTick,
}

impl std::fmt::Display for OpenDurationClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenDurationClass::SingleTick => write!(f, "single_tick"),
            OpenDurationClass::MultiTick => write!(f, "multi_tick"),
        }
    }
}

/// Dimension 2 — how did the window close? (multi_tick only)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    /// Trade event fired, spanned multiple ticks. Priority 1 — best target.
    VolumeSpikeGradual,
    /// Trade event fired on closing tick only. Priority 3 — real but need speed.
    VolumeSpikeInstant,
    /// Spread closed gradually, no trade event. Priority 2 — long window, low competition.
    PriceDrift,
    /// Order disappeared, no trade, no drift. Priority 4 — manually cancelled.
    OrderVanished,
}

impl std::fmt::Display for CloseReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            CloseReason::VolumeSpikeGradual => "volume_spike_gradual",
            CloseReason::VolumeSpikeInstant => "volume_spike_instant",
            CloseReason::PriceDrift => "price_drift",
            CloseReason::OrderVanished => "order_vanished",
        };
        write!(f, "{s}")
    }
}

/// Combined opportunity priority (1=best, 4=lowest, 0=noise/ignore).
pub fn opportunity_class(open_class: OpenDurationClass, close_reason: Option<CloseReason>) -> u8 {
    match (open_class, close_reason) {
        (OpenDurationClass::SingleTick, _) => 0,
        (OpenDurationClass::MultiTick, Some(CloseReason::VolumeSpikeGradual)) => 1,
        (OpenDurationClass::MultiTick, Some(CloseReason::PriceDrift)) => 2,
        (OpenDurationClass::MultiTick, Some(CloseReason::VolumeSpikeInstant)) => 3,
        (OpenDurationClass::MultiTick, Some(CloseReason::OrderVanished)) => 4,
        (OpenDurationClass::MultiTick, None) => 4,
    }
}

// ---------------------------------------------------------------------------
// Raw observables stored alongside every window
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WindowObservables {
    pub tick_count: u32,
    /// True if a last_trade_price event fired while this window was open.
    pub trade_event_fired: bool,
    /// Number of ticks the volume change spanned (0 if no trade event).
    pub volume_change_ticks: u32,
    /// True if ask price moved gradually before close.
    pub price_shifted: bool,
}

// ---------------------------------------------------------------------------
// Window events — sent over mpsc channels between tasks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WindowOpenEvent {
    pub market_id: String,
    pub yes_ask: f64,
    pub no_ask: f64,
    pub spread: f64,
    pub spread_category: SpreadCategory,
    /// Nanosecond UTC epoch timestamp.
    pub opened_at_ns: u64,
    /// For latency measurement — not sent over channel.
    pub detected_at: Instant,
}

#[derive(Debug, Clone)]
pub struct WindowCloseEvent {
    pub market_id: String,
    pub yes_ask: f64,
    pub no_ask: f64,
    pub spread: f64,
    pub spread_category: SpreadCategory,
    pub opened_at_ns: u64,
    pub closed_at_ns: u64,
    pub duration_ms: f64,
    pub open_duration_class: OpenDurationClass,
    pub close_reason: Option<CloseReason>,
    pub opportunity_class: u8,
    pub observables: WindowObservables,
}

#[derive(Debug, Clone)]
pub enum WindowEvent {
    Open(WindowOpenEvent),
    Close(WindowCloseEvent),
}

// ---------------------------------------------------------------------------
// Channel message types
// ---------------------------------------------------------------------------

/// Routed from WS manager to the spread detector.
#[derive(Debug, Clone)]
pub struct PriceChangeMsg {
    pub asset_id: String,
    pub best_ask: f64,
    pub best_bid: f64,
    /// Nanosecond UTC epoch of when message was received.
    pub received_at_ns: u64,
    pub received_at: Instant,
}

/// Routed from WS manager to the trade event handler.
#[derive(Debug, Clone)]
pub struct TradeMsg {
    pub asset_id: String,
    pub price: f64,
    pub received_at_ns: u64,
}

/// Control messages for dynamic market subscription management.
#[derive(Debug)]
pub enum ControlMsg {
    Subscribe(Vec<Market>),
    Unsubscribe(String),
}
