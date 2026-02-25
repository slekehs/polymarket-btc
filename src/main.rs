mod config;
mod db;
mod detector;
mod error;
mod fetcher;
mod market_refresh;
mod scorer;
mod state;
mod types;
mod ws;
mod api;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::api::routes::{ApiState, router};
use crate::config::{Config, CHANNEL_CAPACITY};
use crate::db::writer::DbWriter;
use crate::detector::SpreadDetector;
use crate::error::Result;
use crate::fetcher::{audit_book_prices, fetch_markets};
use crate::market_refresh::{MarketRefresher, PinnedMarketWatcher};
use crate::scorer::MarketScorer;
use crate::state::MarketStore;
use crate::types::{WindowCloseEvent, WindowEvent, WindowOpenEvent};
use crate::ws::WsManager;

#[tokio::main]
async fn main() {
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Config error: {e}");
            std::process::exit(1);
        }
    };

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(&cfg.log_level))
        .init();

    if let Err(e) = run(cfg).await {
        error!("Fatal error: {e}");
        std::process::exit(1);
    }
}

async fn run(cfg: Config) -> Result<()> {
    // --- Database setup ---
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", cfg.db_path)).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    info!("Database ready at {}", cfg.db_path);

    // --- REST bootstrap: fetch filtered active markets ---
    let (markets, stats) = fetch_markets(&cfg).await?;
    info!(
        "Bootstrap complete: {} markets from {} API results (min_vol=${:.0}, min_liq=${:.0}, expiry={:.0}m-{:.0}h)",
        markets.len(),
        stats.api_total,
        cfg.scanner_min_volume_24h,
        cfg.scanner_min_liquidity,
        cfg.scanner_min_expiry_minutes,
        cfg.scanner_max_expiry_hours,
    );
    info!(
        "[FILTER] rejected: no_tokens={} no_outcomes={} low_volume={} low_liquidity={} expiry={}",
        stats.rejected_no_tokens,
        stats.rejected_no_outcomes,
        stats.rejected_low_volume,
        stats.rejected_low_liquidity,
        stats.rejected_expiry,
    );
    if !stats.outcome_samples.is_empty() {
        info!("[FILTER] sample of {} markets rejected by outcomes (showing labels we don't recognize):", stats.outcome_samples.len());
        for (q, outcomes) in &stats.outcome_samples {
            let q_short = if q.len() > 60 { &q[..60] } else { q };
            info!("[FILTER]   \"{q_short}\" → outcomes: {outcomes:?}");
        }
    }

    // --- In-memory market store ---
    let store = MarketStore::new();
    store.add_markets(markets.clone());

    // Persist market metadata to DB
    let created_at = now_ns() as i64;
    for market in &markets {
        let category = market.category.to_string();
        sqlx::query!(
            r#"
            INSERT OR IGNORE INTO markets (id, question, category, end_date_iso, total_volume, created_at)
            VALUES (?, ?, ?, ?, ?, ?)
            "#,
            market.id,
            market.question,
            category,
            market.end_date_iso,
            market.total_volume,
            created_at,
        )
        .execute(&pool)
        .await?;
    }
    info!("Persisted {} markets to DB", markets.len());

    // --- Pinned market notice ---
    if cfg.pinned_slugs.is_empty() {
        warn!("PINNED_SLUGS not set — short-timeframe markets will not be tracked. Example: PINNED_SLUGS=btc-updown-5m,btc-updown-15m,btc-updown-1h,...");
    } else {
        info!("Pinned slugs configured ({}): PinnedMarketWatcher will subscribe on first tick.", cfg.pinned_slugs.join(", "));
    }

    // --- Channels ---
    let (price_tx, price_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let (trade_tx, trade_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let (window_tx, window_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let (control_tx, control_rx) = mpsc::channel::<crate::types::ControlMsg>(CHANNEL_CAPACITY);

    // --- Spawn tasks ---

    // WebSocket manager
    let ws_manager = WsManager::new(
        cfg.ws_url.clone(),
        Arc::clone(&store),
        price_tx,
        trade_tx,
        control_rx,
    );
    tokio::spawn(async move { ws_manager.run().await });

    // Spread detector (hot path)
    let detector = SpreadDetector::new(
        Arc::clone(&store),
        price_rx,
        trade_rx,
        window_tx,
    );
    tokio::spawn(async move { detector.run().await });

    // Window event consumer: telemetry logger + DB writer
    let pool_clone = pool.clone();
    tokio::spawn(async move {
        window_consumer(window_rx, pool_clone).await;
    });

    // Market scorer (background, every 60s)
    let scorer = MarketScorer::new(pool.clone());
    tokio::spawn(async move { scorer.run().await });

    // Market refresher (background, every 300s)
    let pinned_control_tx = control_tx.clone();
    let refresher = MarketRefresher::new(cfg.clone(), Arc::clone(&store), control_tx, pool.clone());
    tokio::spawn(async move { refresher.run().await });

    // Book price audit (one-shot, runs 20s after startup to let WS hydrate)
    let audit_store = Arc::clone(&store);
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        audit_book_prices(&audit_store, 5).await;
    });

    // Pinned market watcher (background, every 30s)
    let pinned_watcher = PinnedMarketWatcher::new(
        cfg.clone(),
        Arc::clone(&store),
        pinned_control_tx,
        pool.clone(),
    );
    tokio::spawn(async move { pinned_watcher.run().await });

    // HTTP API server
    let api_state = ApiState { pool: pool.clone() };
    let app = router(api_state);
    let bind_addr = format!("0.0.0.0:{}", cfg.api_port);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!("HTTP API listening on {bind_addr}");

    axum::serve(listener, app).await?;

    Ok(())
}

/// Consumes WindowEvents: logs to console and writes closes to DB.
async fn window_consumer(
    mut rx: mpsc::Receiver<WindowEvent>,
    pool: sqlx::SqlitePool,
) {
    let db_writer_tx = {
        let (tx, window_rx) = mpsc::channel::<WindowEvent>(CHANNEL_CAPACITY);
        let writer = DbWriter::new(pool, window_rx);
        tokio::spawn(async move { writer.run().await });
        tx
    };

    while let Some(event) = rx.recv().await {
        match &event {
            WindowEvent::Open(o) => log_window_open(o),
            WindowEvent::Close(c) => {
                log_window_close(c);
                if let Err(e) = db_writer_tx.try_send(event) {
                    warn!("DB writer channel full: {e}");
                }
            }
        }
    }
}

fn log_window_open(o: &WindowOpenEvent) {
    info!(
        event = "WINDOW_OPEN",
        market_id = %o.market_id,
        spread = o.spread,
        yes_ask = o.yes_ask,
        no_ask = o.no_ask,
        category = %o.spread_category,
        "WINDOW OPEN  | spread: ${:.4} | yes_ask: {:.4} | no_ask: {:.4} | category: {}",
        o.spread, o.yes_ask, o.no_ask, o.spread_category,
    );
}

fn log_window_close(c: &WindowCloseEvent) {
    let priority_label = match c.opportunity_class {
        1 => "1 ← BEST TARGET",
        2 => "2 ← GOOD TARGET",
        3 => "3 ← FAST REQUIRED",
        4 => "4 ← LOW PRIORITY",
        _ => "0 ← NOISE, IGNORED",
    };
    let close_reason_str = c
        .close_reason
        .map(|r| r.to_string())
        .unwrap_or_else(|| "n/a".to_string());

    info!(
        event = "WINDOW_CLOSE",
        market_id = %c.market_id,
        duration_ms = c.duration_ms,
        tick_count = c.observables.tick_count,
        open_class = %c.open_duration_class,
        close_reason = %close_reason_str,
        priority = c.opportunity_class,
        "WINDOW CLOSE | duration: {:.0}ms | ticks: {} | open_class: {} | close_reason: {} | priority: {}",
        c.duration_ms, c.observables.tick_count, c.open_duration_class, close_reason_str, priority_label,
    );
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}
