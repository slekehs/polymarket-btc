use tokio::sync::mpsc;
use tracing::error;

use crate::error::Result;
use crate::types::{WindowCloseEvent, WindowEvent};

/// Receives WindowEvents from the detector and persists them to SQLite.
/// Runs as a dedicated background task — never blocks the detection path.
pub struct DbWriter {
    pool: sqlx::SqlitePool,
    window_rx: mpsc::Receiver<WindowEvent>,
}

impl DbWriter {
    pub fn new(pool: sqlx::SqlitePool, window_rx: mpsc::Receiver<WindowEvent>) -> Self {
        Self { pool, window_rx }
    }

    pub async fn run(mut self) {
        while let Some(event) = self.window_rx.recv().await {
            if let WindowEvent::Close(close) = event {
                if let Err(e) = self.write_window(&close).await {
                    error!("DB write error: {e}");
                }
            }
        }
    }

    async fn write_window(&self, w: &WindowCloseEvent) -> Result<()> {
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

        sqlx::query!(
            r#"
            INSERT INTO windows (
                market_id, opened_at, closed_at, duration_ms,
                yes_ask, no_ask, combined_cost, spread_size, spread_category,
                open_duration_class, close_reason,
                tick_count, volume_changed, volume_change_ticks, price_shifted,
                opportunity_class
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}
