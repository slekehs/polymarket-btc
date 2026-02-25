use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::error::AppError;

#[derive(Clone)]
pub struct ApiState {
    pub pool: sqlx::SqlitePool,
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/markets", get(get_markets))
        .route("/markets/:id/windows", get(get_market_windows))
        .route("/windows/recent", get(get_recent_windows))
        .route("/stats/summary", get(get_stats_summary))
        .route("/stats/latency", get(get_stats_latency))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Query param structs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct MarketsQuery {
    pub category: Option<String>,
    pub min_score: Option<f64>,
}

#[derive(Deserialize)]
pub struct MarketWindowsQuery {
    pub limit: Option<i64>,
    pub since: Option<i64>,
}

#[derive(Deserialize)]
pub struct RecentWindowsQuery {
    pub min_spread: Option<f64>,
    pub limit: Option<i64>,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
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

#[derive(Serialize)]
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

#[derive(Serialize)]
pub struct SummaryResponse {
    pub total_markets: i64,
    pub windows_today: i64,
    pub avg_duration_ms_today: Option<f64>,
    pub top_markets: Vec<MarketResponse>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn get_markets(
    State(state): State<ApiState>,
    Query(params): Query<MarketsQuery>,
) -> Result<Json<Vec<MarketResponse>>, AppError> {
    let min_score = params.min_score.unwrap_or(0.0);

    let rows = sqlx::query!(
        r#"
        SELECT m.id, m.question, m.category,
               ms.windows_24h, ms.avg_window_duration_ms, ms.avg_spread_size,
               ms.noise_ratio, ms.opportunity_score
        FROM markets m
        LEFT JOIN market_stats ms ON m.id = ms.market_id
        WHERE ms.opportunity_score IS NULL OR ms.opportunity_score >= ?
        ORDER BY ms.opportunity_score DESC NULLS LAST
        "#,
        min_score
    )
    .fetch_all(&state.pool)
    .await?;

    let markets: Vec<MarketResponse> = rows
        .into_iter()
        .filter(|r| {
            params.category.as_ref().map_or(true, |c| {
                r.category.as_deref().map_or(false, |cat| cat == c.as_str())
            })
        })
        .map(|r| MarketResponse {
            id: r.id.unwrap_or_default(),
            question: r.question,
            category: r.category,
            windows_24h: r.windows_24h,
            avg_window_duration_ms: r.avg_window_duration_ms,
            avg_spread_size: r.avg_spread_size,
            noise_ratio: r.noise_ratio,
            opportunity_score: r.opportunity_score,
        })
        .collect();

    Ok(Json(markets))
}

async fn get_market_windows(
    State(state): State<ApiState>,
    Path(market_id): Path<String>,
    Query(params): Query<MarketWindowsQuery>,
) -> Result<Json<Vec<WindowResponse>>, AppError> {
    let limit = params.limit.unwrap_or(100);
    let since = params.since.unwrap_or(0);

    let rows = sqlx::query!(
        r#"
        SELECT id, market_id, opened_at, closed_at, duration_ms,
               spread_size, spread_category, open_duration_class, close_reason, opportunity_class
        FROM windows
        WHERE market_id = ? AND opened_at > ?
        ORDER BY opened_at DESC
        LIMIT ?
        "#,
        market_id,
        since,
        limit
    )
    .fetch_all(&state.pool)
    .await?;

    let windows = rows
        .into_iter()
        .map(|r| WindowResponse {
            id: r.id.unwrap_or(0),
            market_id: r.market_id,
            opened_at: r.opened_at,
            closed_at: r.closed_at,
            duration_ms: r.duration_ms,
            spread_size: r.spread_size,
            spread_category: r.spread_category,
            open_duration_class: r.open_duration_class,
            close_reason: r.close_reason,
            opportunity_class: r.opportunity_class,
        })
        .collect();

    Ok(Json(windows))
}

async fn get_recent_windows(
    State(state): State<ApiState>,
    Query(params): Query<RecentWindowsQuery>,
) -> Result<Json<Vec<WindowResponse>>, AppError> {
    let limit = params.limit.unwrap_or(50);
    let min_spread = params.min_spread.unwrap_or(0.0);

    let rows = sqlx::query!(
        r#"
        SELECT id, market_id, opened_at, closed_at, duration_ms,
               spread_size, spread_category, open_duration_class, close_reason, opportunity_class
        FROM windows
        WHERE spread_size >= ?
        ORDER BY opened_at DESC
        LIMIT ?
        "#,
        min_spread,
        limit
    )
    .fetch_all(&state.pool)
    .await?;

    let windows = rows
        .into_iter()
        .map(|r| WindowResponse {
            id: r.id.unwrap_or(0),
            market_id: r.market_id,
            opened_at: r.opened_at,
            closed_at: r.closed_at,
            duration_ms: r.duration_ms,
            spread_size: r.spread_size,
            spread_category: r.spread_category,
            open_duration_class: r.open_duration_class,
            close_reason: r.close_reason,
            opportunity_class: r.opportunity_class,
        })
        .collect();

    Ok(Json(windows))
}

async fn get_stats_summary(
    State(state): State<ApiState>,
) -> Result<Json<SummaryResponse>, AppError> {
    let total_markets: i64 = sqlx::query_scalar!("SELECT COUNT(*) FROM markets")
        .fetch_one(&state.pool)
        .await?;

    // Today in nanoseconds (midnight UTC)
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64;
    let day_ns = 24i64 * 3_600 * 1_000_000_000;
    let today_start = now_ns - day_ns;

    let windows_today: i64 =
        sqlx::query_scalar!("SELECT COUNT(*) FROM windows WHERE opened_at > ?", today_start)
            .fetch_one(&state.pool)
            .await?;

    let avg_duration: Option<f64> = sqlx::query_scalar!(
        r#"SELECT AVG(duration_ms) as "avg: f64" FROM windows WHERE opened_at > ? AND duration_ms IS NOT NULL"#,
        today_start
    )
    .fetch_one(&state.pool)
    .await?;

    let top_rows = sqlx::query!(
        r#"
        SELECT m.id, m.question, m.category,
               ms.windows_24h, ms.avg_window_duration_ms, ms.avg_spread_size,
               ms.noise_ratio, ms.opportunity_score
        FROM markets m
        LEFT JOIN market_stats ms ON m.id = ms.market_id
        ORDER BY ms.opportunity_score DESC NULLS LAST
        LIMIT 10
        "#
    )
    .fetch_all(&state.pool)
    .await?;

    let top_markets = top_rows
        .into_iter()
        .map(|r| MarketResponse {
            id: r.id.unwrap_or_default(),
            question: r.question,
            category: r.category,
            windows_24h: r.windows_24h,
            avg_window_duration_ms: r.avg_window_duration_ms,
            avg_spread_size: r.avg_spread_size,
            noise_ratio: r.noise_ratio,
            opportunity_score: r.opportunity_score,
        })
        .collect();

    Ok(Json(SummaryResponse {
        total_markets,
        windows_today,
        avg_duration_ms_today: avg_duration,
        top_markets,
    }))
}

async fn get_stats_latency(
    State(_state): State<ApiState>,
) -> Json<serde_json::Value> {
    // Placeholder — will be wired to in-memory latency histogram in Phase 1C
    Json(serde_json::json!({
        "note": "latency histogram not yet implemented",
        "p50_ms": null,
        "p95_ms": null,
        "p99_ms": null
    }))
}
