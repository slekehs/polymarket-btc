use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{error, info};

use crate::config::SCORER_INTERVAL_SECS;
use crate::error::Result;

/// Background task that scores markets every 60 seconds.
/// Reads from SQLite windows table, computes composite scores,
/// and upserts into market_stats.
pub struct MarketScorer {
    pool: sqlx::SqlitePool,
}

impl MarketScorer {
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn run(self) {
        let mut interval = tokio::time::interval(Duration::from_secs(SCORER_INTERVAL_SECS));
        interval.tick().await; // consume immediate first tick

        loop {
            interval.tick().await;
            if let Err(e) = self.score_all_markets().await {
                error!("Scorer error: {e}");
            }
        }
    }

    async fn score_all_markets(&self) -> Result<()> {
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64;

        // 24 hours in nanoseconds
        let window_24h = 24i64 * 3_600 * 1_000_000_000;
        let since = now_ns - window_24h;

        let rows = sqlx::query!(
            r#"
            SELECT
                market_id,
                COUNT(*) as windows_24h,
                SUM(CASE WHEN opportunity_class = 1 THEN 1 ELSE 0 END) as p1_windows_24h,
                SUM(CASE WHEN opportunity_class = 2 THEN 1 ELSE 0 END) as p2_windows_24h,
                AVG(duration_ms) as avg_duration_ms,
                AVG(spread_size) as avg_spread,
                MAX(spread_size) as max_spread,
                CAST(SUM(CASE WHEN open_duration_class = 'single_tick' THEN 1 ELSE 0 END) AS REAL)
                    / CAST(COUNT(*) AS REAL) as noise_ratio
            FROM windows
            WHERE opened_at > ?
            GROUP BY market_id
            "#,
            since
        )
        .fetch_all(&self.pool)
        .await?;

        for row in &rows {
            let avg_duration = row.avg_duration_ms.unwrap_or(0.0);
            let avg_spread = row.avg_spread;
            let noise = row.noise_ratio;
            let p1 = row.p1_windows_24h;
            let p2 = row.p2_windows_24h;
            let score = compute_score(
                row.windows_24h,
                p1,
                p2,
                avg_duration,
                avg_spread,
                noise,
            );

            sqlx::query!(
                r#"
                INSERT INTO market_stats (
                    market_id, windows_24h, p1_windows_24h, p2_windows_24h,
                    avg_window_duration_ms, avg_spread_size, max_spread_size, noise_ratio,
                    opportunity_score, last_updated
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(market_id) DO UPDATE SET
                    windows_24h = excluded.windows_24h,
                    p1_windows_24h = excluded.p1_windows_24h,
                    p2_windows_24h = excluded.p2_windows_24h,
                    avg_window_duration_ms = excluded.avg_window_duration_ms,
                    avg_spread_size = excluded.avg_spread_size,
                    max_spread_size = excluded.max_spread_size,
                    noise_ratio = excluded.noise_ratio,
                    opportunity_score = excluded.opportunity_score,
                    last_updated = excluded.last_updated
                "#,
                row.market_id,
                row.windows_24h,
                p1,
                p2,
                row.avg_duration_ms,
                row.avg_spread,
                row.max_spread,
                row.noise_ratio,
                score,
                now_ns,
            )
            .execute(&self.pool)
            .await?;
        }

        info!("Scorer updated stats for {} markets", rows.len());
        Ok(())
    }
}

/// Composite opportunity score (higher = better market to watch).
/// Factors: quality-weighted window frequency (P1=2x, P2=1.5x), duration, spread, noise.
fn compute_score(
    windows_24h: i64,
    p1_windows: i64,
    p2_windows: i64,
    avg_duration_ms: f64,
    avg_spread: f64,
    noise_ratio: f64,
) -> f64 {
    // Quality-weighted frequency: P1 counts 2x, P2 counts 1.5x
    let other_windows = (windows_24h - p1_windows - p2_windows).max(0) as f64;
    let weighted_count = (p1_windows as f64) * 2.0 + (p2_windows as f64) * 1.5 + other_windows;
    let frequency_score = weighted_count.min(75.0) / 75.0 * 30.0;
    let duration_score = (avg_duration_ms / 2000.0).min(1.0) * 30.0;
    let spread_score = (avg_spread / 0.10).min(1.0) * 25.0;
    let noise_penalty = noise_ratio * 15.0;

    (frequency_score + duration_score + spread_score - noise_penalty).max(0.0)
}
