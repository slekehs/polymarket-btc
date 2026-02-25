use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use tokio::time::interval;
use tracing::{error, info, warn};

use crate::config::{Config, MARKET_REFRESH_INTERVAL_SECS};
use crate::fetcher::{fetch_markets, fetch_pinned_markets, parse_prefix_duration_secs};
use crate::state::MarketStore;
use crate::types::{ControlMsg, Market};

pub struct MarketRefresher {
    cfg: Config,
    store: Arc<MarketStore>,
    control_tx: mpsc::Sender<ControlMsg>,
    pool: sqlx::SqlitePool,
}

impl MarketRefresher {
    pub fn new(
        cfg: Config,
        store: Arc<MarketStore>,
        control_tx: mpsc::Sender<ControlMsg>,
        pool: sqlx::SqlitePool,
    ) -> Self {
        Self { cfg, store, control_tx, pool }
    }

    pub async fn run(self) {
        let mut ticker = interval(Duration::from_secs(MARKET_REFRESH_INTERVAL_SECS));
        ticker.tick().await; // skip immediate first tick — bootstrap already ran

        loop {
            ticker.tick().await;
            if let Err(e) = self.refresh().await {
                error!("Market refresh failed: {e}");
            }
        }
    }

    async fn refresh(&self) -> crate::error::Result<()> {
        let fresh_markets = fetch_markets(&self.cfg).await?;

        let current_ids: HashSet<String> = self.store.all_market_ids().into_iter().collect();
        let fresh_ids: HashSet<String> = fresh_markets.iter().map(|m| m.id.clone()).collect();

        // Markets to remove: currently tracked but no longer in the fresh qualifying set.
        // Pinned markets are never removed — the PinnedMarketWatcher manages them.
        let to_remove: Vec<String> = current_ids
            .difference(&fresh_ids)
            .filter(|id| !self.store.is_pinned(id))
            .cloned()
            .collect();

        // Markets to add: in fresh set but not currently tracked
        let to_add: Vec<_> = fresh_markets
            .into_iter()
            .filter(|m| !current_ids.contains(&m.id))
            .collect();

        let removed_count = to_remove.len();
        let added_count = to_add.len();
        let unchanged_count = current_ids.len().saturating_sub(removed_count);

        for market_id in &to_remove {
            // Send Unsubscribe BEFORE removing from the store — the WS handler
            // calls token_ids_for_market() to build the unsub frame, which requires
            // the market to still be present in the store.
            if let Err(e) = self.control_tx.send(ControlMsg::Unsubscribe(market_id.clone())).await {
                warn!("Failed to send Unsubscribe for {market_id}: {e}");
            }
            self.store.remove_market(market_id);
        }

        if !to_add.is_empty() {
            let created_at = now_ns() as i64;
            for market in &to_add {
                let category = market.category.to_string();
                if let Err(e) = sqlx::query!(
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
                .execute(&self.pool)
                .await
                {
                    warn!("DB insert failed for market {}: {e}", market.id);
                }

                self.store.add_market(market.clone());
            }

            if let Err(e) = self.control_tx.send(ControlMsg::Subscribe(to_add)).await {
                warn!("Failed to send Subscribe batch: {e}");
            }
        }

        info!(
            added = added_count,
            removed = removed_count,
            unchanged = unchanged_count,
            total = self.store.market_count(),
            "Market refresh complete: +{added_count} added, -{removed_count} removed, {unchanged_count} unchanged",
        );

        Ok(())
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// PinnedMarketWatcher
// ---------------------------------------------------------------------------

/// A single fetched pinned market with its resolved end timestamp.
struct KnownPinned {
    market: Market,
    prefix: String,
    end_ts: u64,
}

/// Manages pinned slug market subscriptions with precise lifecycle control:
///
/// - Only the *current* market per prefix is subscribed at any time (the one with
///   the smallest end_ts still in the future).
/// - Pre-subscribes the *next* market 30 seconds before the current one expires.
/// - Unsubscribes and removes markets after they expire (60s grace period).
/// - Re-fetches from Gamma every 30s to discover newly-created markets.
///
/// Ticks every 10 seconds for responsive handoff timing.
pub struct PinnedMarketWatcher {
    cfg: Config,
    store: Arc<MarketStore>,
    control_tx: mpsc::Sender<ControlMsg>,
    pool: sqlx::SqlitePool,
    /// All fetched pinned markets, not yet necessarily subscribed.
    known: HashMap<String, Vec<KnownPinned>>,
    /// Market IDs currently subscribed via WS (and present in store).
    subscribed: HashSet<String>,
    /// Unix seconds of last Gamma fetch.
    last_fetch_secs: u64,
}

/// Grace period after end_ts before we unsubscribe (seconds).
const EXPIRY_GRACE_SECS: u64 = 60;
/// Pre-subscribe next market this many seconds before current expires.
const PRESUB_SECS: u64 = 30;
/// How often the watcher ticks (seconds).
const WATCHER_TICK_SECS: u64 = 10;
/// How often to re-fetch from Gamma (seconds).
const GAMMA_REFETCH_SECS: u64 = 30;

impl PinnedMarketWatcher {
    pub fn new(
        cfg: Config,
        store: Arc<MarketStore>,
        control_tx: mpsc::Sender<ControlMsg>,
        pool: sqlx::SqlitePool,
    ) -> Self {
        Self {
            cfg,
            store,
            control_tx,
            pool,
            known: HashMap::new(),
            subscribed: HashSet::new(),
            last_fetch_secs: 0,
        }
    }

    pub async fn run(mut self) {
        if self.cfg.pinned_slugs.is_empty() {
            return;
        }

        let mut ticker = interval(Duration::from_secs(WATCHER_TICK_SECS));

        loop {
            ticker.tick().await;
            if let Err(e) = self.tick().await {
                error!("Pinned watcher tick failed: {e}");
            }
        }
    }

    async fn tick(&mut self) -> crate::error::Result<()> {
        let now = now_secs();

        // Re-fetch from Gamma periodically to pick up newly-created markets.
        if now.saturating_sub(self.last_fetch_secs) >= GAMMA_REFETCH_SECS {
            self.fetch_known().await?;
            self.last_fetch_secs = now;
        }

        self.manage_subscriptions(now).await
    }

    async fn fetch_known(&mut self) -> crate::error::Result<()> {
        let results = fetch_pinned_markets(&self.cfg, &self.cfg.pinned_slugs).await?;

        self.known.clear();
        for (market, prefix, end_ts) in results {
            self.known
                .entry(prefix.clone())
                .or_default()
                .push(KnownPinned { market, prefix, end_ts });
        }

        // Sort each prefix group by end_ts ascending (current first).
        for markets in self.known.values_mut() {
            markets.sort_by_key(|m| m.end_ts);
        }

        Ok(())
    }

    async fn manage_subscriptions(&mut self, now: u64) -> crate::error::Result<()> {
        let mut desired: HashSet<String> = HashSet::new();

        for (prefix, markets) in &self.known {
            let duration = parse_prefix_duration_secs(prefix);

            // Active = not yet past grace period. Sorted ascending by end_ts.
            let active: Vec<&KnownPinned> = markets
                .iter()
                .filter(|m| m.end_ts + EXPIRY_GRACE_SECS > now)
                .collect();

            if let Some(current) = active.first() {
                desired.insert(current.market.id.clone());

                // Pre-subscribe the next market when current is within PRESUB_SECS of expiry.
                // end_ts is when the resolution window closes; trading window closes at
                // end_ts - duration. We pre-sub 30s before the resolution completes.
                let secs_until_end = current.end_ts.saturating_sub(now);
                if secs_until_end <= PRESUB_SECS + duration {
                    if let Some(next) = active.get(1) {
                        desired.insert(next.market.id.clone());
                    }
                }
            }
        }

        // Markets to subscribe: desired but not yet subscribed.
        let to_subscribe: Vec<Market> = desired
            .iter()
            .filter(|id| !self.subscribed.contains(*id))
            .filter_map(|id| {
                self.known
                    .values()
                    .flat_map(|v| v.iter())
                    .find(|m| &m.market.id == id)
                    .map(|m| m.market.clone())
            })
            .collect();

        // Markets to unsubscribe: currently subscribed but not in desired.
        let to_unsubscribe: Vec<String> = self
            .subscribed
            .iter()
            .filter(|id| !desired.contains(*id))
            .cloned()
            .collect();

        // --- Execute subscribes ---
        if !to_subscribe.is_empty() {
            let created_at = now_ns() as i64;
            for market in &to_subscribe {
                let category = market.category.to_string();
                if let Err(e) = sqlx::query!(
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
                .execute(&self.pool)
                .await
                {
                    warn!("Pinned DB insert failed for {}: {e}", market.id);
                }
                self.store.add_market(market.clone());
                self.store.pin_market(&market.id);
                self.subscribed.insert(market.id.clone());
                info!(
                    market_id = %market.id,
                    question = %market.question,
                    "Pinned SUBSCRIBED: {}",
                    market.question,
                );
            }
            if let Err(e) = self.control_tx.send(ControlMsg::Subscribe(to_subscribe)).await {
                warn!("Failed to send Subscribe for pinned markets: {e}");
            }
        }

        // --- Execute unsubscribes ---
        for market_id in &to_unsubscribe {
            if let Err(e) = self.control_tx.send(ControlMsg::Unsubscribe(market_id.clone())).await {
                warn!("Failed to send Unsubscribe for pinned market {market_id}: {e}");
            }
            self.store.remove_market(market_id);
            self.subscribed.remove(market_id);
            info!(market_id = %market_id, "Pinned EXPIRED + unsubscribed: {market_id}");
        }

        Ok(())
    }
}
