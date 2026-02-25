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
    pub p1_windows_24h: Option<i64>,
    pub p2_windows_24h: Option<i64>,
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
    pub detection_latency_us: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct HealthResponse {
    pub ws_connected: Option<bool>,
    pub markets_subscribed: Option<i64>,
    pub hydrated_markets: Option<i64>,
    pub total_markets: Option<i64>,
    pub last_window_at_ns: Option<i64>,
    pub write_queue_pending: Option<i64>,
    pub detection_p99_us: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct LatencyResponse {
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
    pub sample_count: Option<i64>,
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

/// Windows for a specific market (from GET /markets/:id/windows).
#[derive(Debug, Clone, Default)]
pub struct MarketWindowsState {
    pub market_id: Option<String>,
    pub market_question: Option<String>,
    pub windows: Vec<WindowResponse>,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub status: ConnectionStatus,
    pub summary: SummaryResponse,
    pub markets: Vec<MarketResponse>,
    pub recent_windows: Vec<WindowResponse>,
    pub open_windows: Vec<WindowResponse>,
    pub market_windows: MarketWindowsState,
    pub health: HealthResponse,
    pub latency: LatencyResponse,
    pub last_refresh: std::time::Instant,
    pub base_url: String,
}

impl AppState {
    pub fn new(base_url: String) -> Self {
        Self {
            status: ConnectionStatus::Connecting,
            summary: SummaryResponse::default(),
            markets: Vec::new(),
            recent_windows: Vec::new(),
            open_windows: Vec::new(),
            market_windows: MarketWindowsState::default(),
            health: HealthResponse::default(),
            latency: LatencyResponse::default(),
            last_refresh: std::time::Instant::now(),
            base_url,
        }
    }

    /// Fetch arb windows for a specific market and store in market_windows.
    pub async fn fetch_market_windows(&mut self, client: &reqwest::Client, market_id: &str) {
        let url = format!("{}/markets/{}/windows?limit=100", self.base_url, market_id);
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(windows) = resp.json::<Vec<WindowResponse>>().await {
                    let question = self
                        .markets
                        .iter()
                        .find(|m| m.id == market_id)
                        .map(|m| m.question.clone());
                    self.market_windows = MarketWindowsState {
                        market_id: Some(market_id.to_string()),
                        market_question: question,
                        windows,
                    };
                }
            }
            _ => {}
        }
    }

    /// Clear market detail view and show recent windows again.
    pub fn clear_market_windows(&mut self) {
        self.market_windows = MarketWindowsState::default();
    }

    pub fn showing_market_windows(&self) -> bool {
        self.market_windows.market_id.is_some()
    }

    /// Windows to display on the right pane (market detail or recent).
    pub fn displayed_windows(&self) -> &[WindowResponse] {
        if self.showing_market_windows() {
            &self.market_windows.windows
        } else {
            &self.recent_windows
        }
    }

    pub async fn refresh(&mut self, client: &reqwest::Client) {
        let summary_url = format!("{}/stats/summary", self.base_url);
        let windows_url = format!("{}/windows/recent?limit=100", self.base_url);
        let markets_url = format!("{}/markets", self.base_url);
        let health_url = format!("{}/health", self.base_url);
        let latency_url = format!("{}/stats/latency", self.base_url);
        let open_url = format!("{}/windows/open", self.base_url);

        let (summary_res, windows_res, markets_res, health_res, latency_res, open_res) = tokio::join!(
            client.get(&summary_url).send(),
            client.get(&windows_url).send(),
            client.get(&markets_url).send(),
            client.get(&health_url).send(),
            client.get(&latency_url).send(),
            client.get(&open_url).send(),
        );

        let core_ok = summary_res.is_ok() && windows_res.is_ok() && markets_res.is_ok();
        if !core_ok {
            let err = summary_res.err().or_else(|| windows_res.err()).or_else(|| markets_res.err());
            if let Some(e) = err {
                self.status = ConnectionStatus::Error(format!("{e}"));
            }
            return;
        }

        let (summary, windows, markets) = tokio::join!(
            summary_res.unwrap().json::<SummaryResponse>(),
            windows_res.unwrap().json::<Vec<WindowResponse>>(),
            markets_res.unwrap().json::<Vec<MarketResponse>>(),
        );

        match (summary, windows, markets) {
            (Ok(s), Ok(w), Ok(m)) => {
                self.summary = s;
                self.recent_windows = w;
                self.markets = m;
                self.status = ConnectionStatus::Connected;
                self.last_refresh = std::time::Instant::now();

                if let Ok(h) = health_res {
                    if let Ok(health) = h.json::<HealthResponse>().await {
                        self.health = health;
                    }
                }
                if let Ok(l) = latency_res {
                    if let Ok(latency) = l.json::<LatencyResponse>().await {
                        self.latency = latency;
                    }
                }
                if let Ok(o) = open_res {
                    if let Ok(open) = o.json::<Vec<WindowResponse>>().await {
                        self.open_windows = open;
                    }
                }
            }
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                self.status = ConnectionStatus::Error(format!("parse error: {e}"));
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
