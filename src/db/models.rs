/// Database row types matching the schema in polymarket-scanner-prd-v1.md section 5.
/// Used by sqlx for typed queries.

#[derive(Debug, sqlx::FromRow)]
pub struct MarketRow {
    pub id: String,
    pub question: String,
    pub category: Option<String>,
    pub end_date_iso: Option<String>,
    pub total_volume: Option<f64>,
    pub created_at: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub struct WindowRow {
    pub id: Option<i64>,
    pub market_id: String,
    pub opened_at: i64,
    pub closed_at: Option<i64>,
    pub duration_ms: Option<f64>,
    pub yes_ask: f64,
    pub no_ask: f64,
    pub combined_cost: f64,
    pub spread_size: f64,
    pub spread_category: Option<String>,
    pub open_duration_class: Option<String>,
    pub close_reason: Option<String>,
    pub tick_count: i64,
    pub volume_changed: i64,
    pub volume_change_ticks: Option<i64>,
    pub price_shifted: i64,
    pub opportunity_class: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct MarketStatsRow {
    pub market_id: String,
    pub windows_24h: i64,
    pub avg_window_duration_ms: Option<f64>,
    pub avg_spread_size: Option<f64>,
    pub max_spread_size: Option<f64>,
    pub noise_ratio: Option<f64>,
    pub opportunity_score: Option<f64>,
    pub last_updated: i64,
}
