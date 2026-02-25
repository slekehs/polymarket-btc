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
            let score = compute_score(row.windows_24h, avg_duration, avg_spread, noise);

            sqlx::query!(
                r#"
                INSERT INTO market_stats (
                    market_id, windows_24h, avg_window_duration_ms,
                    avg_spread_size, max_spread_size, noise_ratio,
                    opportunity_score, last_updated
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(market_id) DO UPDATE SET
                    windows_24h = excluded.windows_24h,
                    avg_window_duration_ms = excluded.avg_window_duration_ms,
                    avg_spread_size = excluded.avg_spread_size,
                    max_spread_size = excluded.max_spread_size,
                    noise_ratio = excluded.noise_ratio,
                    opportunity_score = excluded.opportunity_score,
                    last_updated = excluded.last_updated
                "#,
                row.market_id,
                row.windows_24h,
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
/// Factors: window frequency, average duration, spread size, noise ratio.
fn compute_score(windows_24h: i64, avg_duration_ms: f64, avg_spread: f64, noise_ratio: f64) -> f64 {
    // Normalize and weight each factor
    let frequency_score = (windows_24h as f64).min(50.0) / 50.0 * 30.0;
    let duration_score = (avg_duration_ms / 2000.0).min(1.0) * 30.0;
    let spread_score = (avg_spread / 0.10).min(1.0) * 25.0;
    let noise_penalty = noise_ratio * 15.0;

    (frequency_score + duration_score + spread_score - noise_penalty).max(0.0)
}
