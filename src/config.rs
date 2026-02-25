use crate::error::{AppError, Result};

pub const WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
pub const GAMMA_API_URL: &str = "https://gamma-api.polymarket.com";
pub const CLOB_API_URL: &str = "https://clob.polymarket.com";

/// Minimum consecutive ticks a spread must survive before being registered as a real window.
/// A window with tick_count < MIN_ARB_TICKS is classified as single_tick (noise).
/// Must be >= 2 — the open event fires in the (true, true) branch so tick_count=1 can never
/// reach the confirmation check.
pub const MIN_ARB_TICKS: u32 = 2;

/// Heartbeat ping interval (seconds).
pub const WS_PING_INTERVAL_SECS: u64 = 30;

/// Alert threshold: no messages received for this many seconds on an active market.
pub const WS_SILENCE_ALERT_SECS: u64 = 5;

/// Reconnect backoff values in milliseconds.
pub const RECONNECT_BACKOFF_MS: &[u64] = &[100, 200, 400, 800];

/// Channel capacity for internal message routing.
pub const CHANNEL_CAPACITY: usize = 1024;

/// Market scorer update interval (seconds).
pub const SCORER_INTERVAL_SECS: u64 = 60;

/// Market refresh interval (seconds) — how often to re-fetch qualifying markets from Gamma.
pub const MARKET_REFRESH_INTERVAL_SECS: u64 = 300;


/// Maximum asset IDs per WS subscribe frame to avoid server-side size limits.
pub const WS_SUBSCRIBE_CHUNK_SIZE: usize = 500;

/// Spread size thresholds (1.00 - combined_cost).
pub mod spread_thresholds {
    pub const NOISE_MAX: f64 = 0.02;
    pub const SMALL_MAX: f64 = 0.05;
    pub const MEDIUM_MAX: f64 = 0.10;
}

#[derive(Debug, Clone)]
pub struct Config {
    pub ws_url: String,
    pub gamma_api_url: String,
    pub log_level: String,
    pub db_path: String,
    pub api_port: u16,
    /// Max markets to subscribe to via WS (SCANNER_MAX_SUBSCRIPTIONS)
    pub scanner_max_markets: usize,
    /// Minimum 24h volume in USD (SCANNER_MIN_VOLUME_24H)
    pub scanner_min_volume_24h: f64,
    /// Minimum liquidity in USD (SCANNER_MIN_LIQUIDITY)
    pub scanner_min_liquidity: f64,
    /// Markets expiring further out than this are excluded (SCANNER_MAX_EXPIRY_HOURS)
    pub scanner_max_expiry_hours: f64,
    /// Markets expiring sooner than this are excluded (SCANNER_MIN_EXPIRY_MINUTES)
    pub scanner_min_expiry_minutes: f64,
    /// Slug prefixes to always track regardless of filters (PINNED_SLUGS, comma-separated).
    /// Example: "btc-updown-5m,btc-updown-15m,eth-updown-5m"
    pub pinned_slugs: Vec<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            ws_url: std::env::var("WS_URL").unwrap_or_else(|_| WS_URL.to_string()),
            gamma_api_url: std::env::var("GAMMA_API_URL")
                .unwrap_or_else(|_| GAMMA_API_URL.to_string()),
            log_level: std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string()),
            db_path: std::env::var("DB_PATH").unwrap_or_else(|_| "scanner.db".to_string()),
            api_port: std::env::var("API_PORT")
                .unwrap_or_else(|_| "3000".to_string())
                .parse::<u16>()
                .map_err(|_| AppError::Config("API_PORT must be a valid port number".to_string()))?,
            scanner_max_markets: std::env::var("SCANNER_MAX_SUBSCRIPTIONS")
                .unwrap_or_else(|_| "200".to_string())
                .parse::<usize>()
                .unwrap_or(500),
            scanner_min_volume_24h: std::env::var("SCANNER_MIN_VOLUME_24H")
                .unwrap_or_else(|_| "10000".to_string())
                .parse::<f64>()
                .unwrap_or(5000.0),
            scanner_min_liquidity: std::env::var("SCANNER_MIN_LIQUIDITY")
                .unwrap_or_else(|_| "1000".to_string())
                .parse::<f64>()
                .unwrap_or(1000.0),
            scanner_max_expiry_hours: std::env::var("SCANNER_MAX_EXPIRY_HOURS")
                .unwrap_or_else(|_| "72".to_string())
                .parse::<f64>()
                .unwrap_or(72.0),
            scanner_min_expiry_minutes: std::env::var("SCANNER_MIN_EXPIRY_MINUTES")
                .unwrap_or_else(|_| "30".to_string())
                .parse::<f64>()
                .unwrap_or(30.0),
            pinned_slugs: std::env::var("PINNED_SLUGS")
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        })
    }
}
