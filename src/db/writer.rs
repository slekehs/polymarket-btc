use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::error;

use crate::api::health::HealthState;
use crate::error::Result;
use crate::types::{WindowCloseEvent, WindowOpenEvent, WindowEvent};

/// Receives WindowEvents from the detector and persists them to SQLite.
/// Runs as a dedicated background task — never blocks the detection path.
pub struct DbWriter {
    pool: sqlx::SqlitePool,
    window_rx: mpsc::Receiver<WindowEvent>,
    health: Arc<HealthState>,
}

impl DbWriter {
    pub fn new(
        pool: sqlx::SqlitePool,
        window_rx: mpsc::Receiver<WindowEvent>,
        health: Arc<HealthState>,
    ) -> Self {
        Self {
            pool,
            window_rx,
            health,
        }
    }

    pub async fn run(mut self) {
        while let Some(event) = self.window_rx.recv().await {
            match event {
                WindowEvent::Open(open) => {
                    if let Err(e) = self.write_window_open(&open).await {
                        error!("DB write error (open): {e}");
                    }
                }
                WindowEvent::Close(close) => {
                    self.health.dec_write_queue_pending();
                    if let Err(e) = self.write_window_close(&close).await {
                        error!("DB write error (close): {e}");
                    }
                }
            }
        }
    }

    async fn write_window_open(&self, o: &WindowOpenEvent) -> Result<()> {
        let spread_category = o.spread_category.to_string();
        let opened_at = o.opened_at_ns as i64;
        let combined_cost = o.yes_ask + o.no_ask;

        sqlx::query!(
            r#"
            INSERT INTO windows (
                market_id, opened_at, closed_at, duration_ms,
                yes_ask, no_ask, combined_cost, spread_size, spread_category
            ) VALUES (?, ?, NULL, NULL, ?, ?, ?, ?, ?)
            "#,
            o.market_id,
            opened_at,
            o.yes_ask,
            o.no_ask,
            combined_cost,
            o.spread,
            spread_category,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// On Close: update existing open row if found, else insert (single-tick case).
    async fn write_window_close(&self, w: &WindowCloseEvent) -> Result<()> {
        let spread_category = w.spread_category.to_string();
        let open_class = w.open_duration_class.to_string();
        let close_reason = w.close_reason.map(|r| r.to_string());
        let volume_changed = i64::from(w.observables.trade_event_fired);
        let price_shifted = i64::from(w.observables.price_shifted);
        let volume_change_ticks = w.observables.volume_change_ticks as i64;
        let opportunity_class = w.opportunity_class as i64;
        let tick_count = w.observables.tick_count as i64;
        let opened_at = w.opened_at_ns as i64;
        let closed_at = w.closed_at_ns as i64;
        let combined_cost = w.yes_ask + w.no_ask;

        let detection_latency_us = w.detection_latency_us as i64;

        // Try to update existing open row first
        let update_result = sqlx::query!(
            r#"
            UPDATE windows
            SET closed_at = ?, duration_ms = ?, open_duration_class = ?, close_reason = ?,
                tick_count = ?, volume_changed = ?, volume_change_ticks = ?, price_shifted = ?,
                opportunity_class = ?, detection_latency_us = ?,
                yes_ask = ?, no_ask = ?, combined_cost = ?, spread_size = ?, spread_category = ?
            WHERE market_id = ? AND opened_at = ? AND closed_at IS NULL
            "#,
            closed_at,
            w.duration_ms,
            open_class,
            close_reason,
            tick_count,
            volume_changed,
            volume_change_ticks,
            price_shifted,
            opportunity_class,
            detection_latency_us,
            w.yes_ask,
            w.no_ask,
            combined_cost,
            w.spread,
            spread_category,
            w.market_id,
            opened_at,
        )
        .execute(&self.pool)
        .await?;

        if update_result.rows_affected() > 0 {
            return Ok(());
        }

        // Single-tick or missed open: insert full row
        sqlx::query!(
            r#"
            INSERT INTO windows (
                market_id, opened_at, closed_at, duration_ms,
                yes_ask, no_ask, combined_cost, spread_size, spread_category,
                open_duration_class, close_reason,
                tick_count, volume_changed, volume_change_ticks, price_shifted,
                opportunity_class, detection_latency_us
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            w.market_id,
            opened_at,
            closed_at,
            w.duration_ms,
            w.yes_ask,
            w.no_ask,
            combined_cost,
            w.spread,
            spread_category,
            open_class,
            close_reason,
            tick_count,
            volume_changed,
            volume_change_ticks,
            price_shifted,
            opportunity_class,
            detection_latency_us,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}
