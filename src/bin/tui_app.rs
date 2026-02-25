use serde::Deserialize;

// ---------------------------------------------------------------------------
// API response types (mirror routes.rs shapes)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct SummaryResponse {
    pub total_markets: i64,
    pub windows_today: i64,
    pub avg_duration_ms_today: Option<f64>,
    pub top_markets: Vec<MarketResponse>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct MarketResponse {
    pub id: String,
    pub question: String,
    pub category: Option<String>,
    pub windows_24h: Option<i64>,
    pub avg_window_duration_ms: Option<f64>,
    pub avg_spread_size: Option<f64>,
    pub noise_ratio: Option<f64>,
    pub opportunity_score: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct WindowResponse {
    pub id: i64,
    pub market_id: String,
    pub opened_at: i64,
    pub closed_at: Option<i64>,
    pub duration_ms: Option<f64>,
    pub spread_size: f64,
    pub spread_category: Option<String>,
    pub open_duration_class: Option<String>,
    pub close_reason: Option<String>,
    pub opportunity_class: Option<i64>,
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionStatus {
    Connected,
    Error(String),
    Connecting,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub status: ConnectionStatus,
    pub summary: SummaryResponse,
    pub markets: Vec<MarketResponse>,
    pub recent_windows: Vec<WindowResponse>,
    pub last_refresh: std::time::Instant,
    #[allow(dead_code)]
    pub market_scroll: usize,
    #[allow(dead_code)]
    pub window_scroll: usize,
    pub base_url: String,
}

impl AppState {
    pub fn new(base_url: String) -> Self {
        Self {
            status: ConnectionStatus::Connecting,
            summary: SummaryResponse::default(),
            markets: Vec::new(),
            recent_windows: Vec::new(),
            last_refresh: std::time::Instant::now(),
            market_scroll: 0,
            window_scroll: 0,
            base_url,
        }
    }

    pub async fn refresh(&mut self, client: &reqwest::Client) {
        let summary_url = format!("{}/stats/summary", self.base_url);
        let windows_url = format!("{}/windows/recent?limit=100", self.base_url);
        let markets_url = format!("{}/markets", self.base_url);

        let summary_res = client.get(&summary_url).send().await;
        let windows_res = client.get(&windows_url).send().await;
        let markets_res = client.get(&markets_url).send().await;

        match (summary_res, windows_res, markets_res) {
            (Ok(s), Ok(w), Ok(m)) => {
                let summary: Result<SummaryResponse, _> = s.json().await;
                let windows: Result<Vec<WindowResponse>, _> = w.json().await;
                let markets: Result<Vec<MarketResponse>, _> = m.json().await;

                match (summary, windows, markets) {
                    (Ok(s), Ok(w), Ok(m)) => {
                        self.summary = s;
                        self.recent_windows = w;
                        self.markets = m;
                        self.status = ConnectionStatus::Connected;
                        self.last_refresh = std::time::Instant::now();
                    }
                    (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                        self.status = ConnectionStatus::Error(format!("parse error: {e}"));
                    }
                }
            }
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                self.status = ConnectionStatus::Error(format!("{e}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

pub fn format_spread(v: f64) -> String {
    format!("+{:.3}", v)
}

pub fn format_duration(ms: Option<f64>) -> String {
    match ms {
        Some(d) if d >= 1000.0 => format!("{:.1}s", d / 1000.0),
        Some(d) => format!("{:.0}ms", d),
        None => "—".to_string(),
    }
}

pub fn format_class(opportunity_class: Option<i64>, open_duration_class: Option<&str>) -> String {
    if open_duration_class == Some("single_tick") {
        return "noise".to_string();
    }
    match opportunity_class {
        Some(c) => format!("P{c}"),
        None => "—".to_string(),
    }
}

/// Convert nanosecond epoch timestamp to HH:MM:SS string.
pub fn format_time_ns(ns: i64) -> String {
    let secs = (ns / 1_000_000_000) as u64;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

fn main() {
    // TUI app is a work in progress — entry point lives in src/bin/tui.rs
}
