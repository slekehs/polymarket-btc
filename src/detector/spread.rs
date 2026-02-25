use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::config::MIN_ARB_TICKS;
use crate::detector::classifier;
use crate::state::MarketStore;
use crate::types::{
    opportunity_class, PriceChangeMsg, SpreadCategory, TradeMsg, WindowCloseEvent, WindowEvent,
    WindowObservables, WindowOpenEvent,
};

/// Tracks state for a currently open arbitrage window.
struct ActiveWindow {
    yes_ask: f64,
    no_ask: f64,
    spread: f64,
    opened_at_ns: u64,
    opened_at: Instant,
    tick_count: u32,
    /// Previous ask to detect gradual price drift
    prev_yes_ask: f64,
    prev_no_ask: f64,
    trade_event_fired: bool,
    volume_change_ticks: u32,
    /// Count of ticks where price moved (not just opened)
    price_shift_ticks: u32,
    /// Whether we're currently in a 2-tick pending state before the window is "open"
    pending: bool,
}

pub struct SpreadDetector {
    store: Arc<MarketStore>,
    price_rx: mpsc::Receiver<PriceChangeMsg>,
    trade_rx: mpsc::Receiver<TradeMsg>,
    window_tx: mpsc::Sender<WindowEvent>,
    /// market_id → active window state
    active_windows: HashMap<String, ActiveWindow>,
    /// Detector-local price cache: asset_id → (best_ask, best_bid).
    /// Ensures spread is computed from prices in strict message order,
    /// avoiding the race where the shared store is updated ahead of us.
    local_prices: HashMap<String, (f64, f64)>,
    /// Count of price_change messages processed (for diagnostics).
    price_msgs_processed: u64,
    /// Whether the 10s readiness snapshot has been logged.
    startup_logged: bool,
    started_at: Instant,
    /// Lifetime counters for window open/close events.
    windows_opened: u64,
    windows_closed: u64,
    /// Track tightest spread seen per 30-second diagnostic window.
    tightest_spread: f64,
    last_diag_at: Instant,
}

impl SpreadDetector {
    pub fn new(
        store: Arc<MarketStore>,
        price_rx: mpsc::Receiver<PriceChangeMsg>,
        trade_rx: mpsc::Receiver<TradeMsg>,
        window_tx: mpsc::Sender<WindowEvent>,
    ) -> Self {
        let now = Instant::now();
        Self {
            store,
            price_rx,
            trade_rx,
            window_tx,
            active_windows: HashMap::new(),
            local_prices: HashMap::new(),
            price_msgs_processed: 0,
            startup_logged: false,
            started_at: now,
            windows_opened: 0,
            windows_closed: 0,
            tightest_spread: f64::NEG_INFINITY,
            last_diag_at: now,
        }
    }

    pub async fn run(mut self) {
        loop {
            tokio::select! {
                Some(msg) = self.price_rx.recv() => {
                    self.handle_price_change(msg).await;
                    self.maybe_log_readiness();
                }
                Some(trade) = self.trade_rx.recv() => {
                    self.handle_trade(trade);
                }
                else => break,
            }
        }
    }

    /// Logs a one-time readiness snapshot 10 seconds after startup to confirm
    /// how many markets have both sides populated in the store.
    fn maybe_log_readiness(&mut self) {
        if self.startup_logged {
            return;
        }
        if self.started_at.elapsed() < Duration::from_secs(10) {
            return;
        }
        self.startup_logged = true;

        let total_markets = self.store.market_count();
        let hydrated = self.store.hydrated_market_count();
        info!(
            total_markets,
            hydrated_markets = hydrated,
            price_msgs_processed = self.price_msgs_processed,
            "[DETECTOR] 10s readiness: {hydrated}/{total_markets} markets have both sides populated | {processed} price msgs processed",
            processed = self.price_msgs_processed,
        );

        self.log_hydration_audit();
    }

    /// One-shot audit at startup: dump full price info for up to 5 hydrated markets.
    fn log_hydration_audit(&self) {
        info!("[HYDRATION AUDIT] Sampling up to 5 hydrated markets...");
        let mut count = 0;
        let ids = self.store.all_asset_ids();
        let mut seen_markets = std::collections::HashSet::new();

        for asset_id in &ids {
            if count >= 5 { break; }
            if let Some((market_id, yes_ask, no_ask, yes_bid, no_bid)) = self.store.get_spread_inputs(asset_id) {
                if !seen_markets.insert(market_id.clone()) { continue; }
                count += 1;

                let combined_ask = yes_ask + no_ask;
                let combined_bid = yes_bid + no_bid;
                let yes_mid = (yes_ask + yes_bid) / 2.0;
                let no_mid = (no_ask + no_bid) / 2.0;
                let combined_mid = yes_mid + no_mid;
                let spread = 1.0 - combined_ask;
                let id_short = if market_id.len() > 12 { &market_id[..12] } else { &market_id };

                info!(
                    "[HYDRATION AUDIT] [{count}/5] {id_short} | \
                     YES: ask={yes_ask:.4} bid={yes_bid:.4} | \
                     NO: ask={no_ask:.4} bid={no_bid:.4} | \
                     combined: ask={combined_ask:.4} bid={combined_bid:.4} mid={combined_mid:.4} | \
                     spread(ask)={spread:.4}"
                );
            }
        }

        if count == 0 {
            warn!("[HYDRATION AUDIT] No markets have both sides populated yet");
        }
    }

    fn maybe_log_diagnostics(&mut self) {
        if self.last_diag_at.elapsed() < Duration::from_secs(30) {
            return;
        }
        let tightest = self.tightest_spread;
        self.tightest_spread = f64::NEG_INFINITY;
        self.last_diag_at = Instant::now();

        info!(
            price_msgs = self.price_msgs_processed,
            opened = self.windows_opened,
            closed = self.windows_closed,
            active = self.active_windows.len(),
            tightest_spread = format_args!("{tightest:.4}"),
            "[DETECTOR] 30s diag | msgs={} open={} close={} active={} tightest_spread={:.4}",
            self.price_msgs_processed, self.windows_opened, self.windows_closed,
            self.active_windows.len(), tightest,
        );

        self.log_sample_market_breakdown();
    }

    /// Logs a full price breakdown for a sample hydrated market so we can
    /// visually verify the ask vs midpoint vs combined numbers.
    fn log_sample_market_breakdown(&self) {
        let ids = self.store.all_asset_ids();
        for asset_id in ids.iter().take(20) {
            if let Some((market_id, yes_ask, no_ask, yes_bid, no_bid)) = self.store.get_spread_inputs(asset_id) {
                let combined_ask = yes_ask + no_ask;
                let yes_mid = (yes_ask + yes_bid) / 2.0;
                let no_mid = (no_ask + no_bid) / 2.0;
                let combined_mid = yes_mid + no_mid;
                let spread_from_asks = 1.0 - combined_ask;
                let spread_from_mids = 1.0 - combined_mid;
                let id_short = if market_id.len() > 12 { &market_id[..12] } else { &market_id };
                info!(
                    "[PRICE SAMPLE] {id_short} | \
                     YES: ask={yes_ask:.4} bid={yes_bid:.4} mid={yes_mid:.4} | \
                     NO: ask={no_ask:.4} bid={no_bid:.4} mid={no_mid:.4} | \
                     combined_ask={combined_ask:.4} combined_mid={combined_mid:.4} | \
                     spread(ask)={spread_from_asks:.4} spread(mid)={spread_from_mids:.4}"
                );
                return;
            }
        }
    }

    async fn handle_price_change(&mut self, msg: PriceChangeMsg) {
        self.price_msgs_processed += 1;

        // Update detector-local price cache (strict message order — no store race).
        self.local_prices.insert(msg.asset_id.clone(), (msg.best_ask, msg.best_bid));

        // Look up market structure (immutable metadata, no price read).
        let Some((market_id, yes_token_id, no_token_id)) = self.store.get_market_for_token(&msg.asset_id) else {
            debug!(
                asset_id = %msg.asset_id,
                "market lookup failed: token not in store"
            );
            return;
        };

        // Read both sides from local cache only — counterpart must have been
        // received through the channel before we can compute a spread.
        let Some(&(yes_ask, _)) = self.local_prices.get(&yes_token_id) else {
            return;
        };
        let Some(&(no_ask, _)) = self.local_prices.get(&no_token_id) else {
            return;
        };

        if yes_ask <= 0.0 || no_ask <= 0.0 {
            return;
        }

        let combined = yes_ask + no_ask;
        let spread = 1.0 - combined;
        let is_arb = spread > 0.0;
        let in_window = self.active_windows.contains_key(&market_id);

        // Track tightest spread for periodic diagnostics.
        if spread > self.tightest_spread {
            self.tightest_spread = spread;
        }
        self.maybe_log_diagnostics();

        let detect_elapsed = msg.received_at.elapsed();

        // Every tick at debug level — use LOG_LEVEL=debug to see the full feed.
        let mid = &market_id;
        let id_short = if mid.len() > 12 { &mid[..12] } else { mid };
        if is_arb {
            debug!(
                "\x1b[32m ARB  | {id_short} | yes={yes_ask:.4} no={no_ask:.4} | combined={combined:.4} spread=+{spread:.4} | {latency}us\x1b[0m",
                latency = detect_elapsed.as_micros(),
            );
        } else {
            debug!(
                " TICK | {id_short} | yes={yes_ask:.4} no={no_ask:.4} | combined={combined:.4} spread={spread:.4} | {latency}us",
                latency = detect_elapsed.as_micros(),
            );
        }

        match (is_arb, in_window) {
            (true, false) => {
                info!(
                    "\x1b[32;1m>>> WINDOW OPENING | {id_short} | yes={yes_ask:.4} no={no_ask:.4} | spread=+{spread:.4}\x1b[0m",
                );
                self.active_windows.insert(market_id.clone(), ActiveWindow {
                    yes_ask,
                    no_ask,
                    spread,
                    opened_at_ns: msg.received_at_ns,
                    opened_at: msg.received_at,
                    tick_count: 1,
                    prev_yes_ask: yes_ask,
                    prev_no_ask: no_ask,
                    trade_event_fired: false,
                    volume_change_ticks: 0,
                    price_shift_ticks: 0,
                    pending: true,
                });
            }

            (true, true) => {
                let window = self.active_windows.get_mut(&market_id).unwrap();
                window.tick_count += 1;

                // Detect gradual price drift: ask moved since last tick
                let yes_drifted = (yes_ask - window.prev_yes_ask).abs() > 1e-6;
                let no_drifted = (no_ask - window.prev_no_ask).abs() > 1e-6;
                if yes_drifted || no_drifted {
                    window.price_shift_ticks += 1;
                }
                window.prev_yes_ask = yes_ask;
                window.prev_no_ask = no_ask;

                // Confirm window open once we hit MIN_ARB_TICKS
                if window.pending && window.tick_count >= MIN_ARB_TICKS {
                    window.pending = false;
                    self.windows_opened += 1;
                    let spread_category = SpreadCategory::from_spread(window.spread);
                    let event = WindowEvent::Open(WindowOpenEvent {
                        market_id: market_id.clone(),
                        yes_ask: window.yes_ask,
                        no_ask: window.no_ask,
                        spread: window.spread,
                        spread_category,
                        opened_at_ns: window.opened_at_ns,
                        detected_at: window.opened_at,
                    });
                    if let Err(e) = self.window_tx.try_send(event) {
                        warn!("window channel full, dropping open event: {e}");
                    }
                }
            }

            (false, true) => {
                self.windows_closed += 1;
                let window = self.active_windows.remove(&market_id).unwrap();
                let dur_ms = (msg.received_at_ns.saturating_sub(window.opened_at_ns)) as f64 / 1_000_000.0;
                info!(
                    "\x1b[31m<<< WINDOW CLOSED  | {id_short} | ticks={} | {dur_ms:.0}ms | spread was +{:.4}\x1b[0m",
                    window.tick_count, window.spread,
                );
                self.emit_close(market_id, window, msg.received_at_ns).await;
            }

            (false, false) => {
                // No spread, no window — nothing to do
            }
        }
    }

    fn handle_trade(&mut self, trade: TradeMsg) {
        if let Some((market_id, _, _)) = self.store.get_market_for_token(&trade.asset_id) {
            if let Some(window) = self.active_windows.get_mut(&market_id) {
                if !window.trade_event_fired {
                    window.trade_event_fired = true;
                    window.volume_change_ticks = 1;
                } else {
                    window.volume_change_ticks += 1;
                }
            }
        }
    }

    async fn emit_close(&self, market_id: String, window: ActiveWindow, closed_at_ns: u64) {
        let duration_ms = (closed_at_ns.saturating_sub(window.opened_at_ns)) as f64 / 1_000_000.0;

        let obs = WindowObservables {
            tick_count: window.tick_count,
            trade_event_fired: window.trade_event_fired,
            volume_change_ticks: window.volume_change_ticks,
            price_shifted: window.price_shift_ticks > 1,
        };

        let (open_class, close_reason) = classifier::classify(&obs);
        let opp_class = opportunity_class(open_class, close_reason);
        let spread_category = SpreadCategory::from_spread(window.spread);

        let event = WindowEvent::Close(WindowCloseEvent {
            market_id,
            yes_ask: window.yes_ask,
            no_ask: window.no_ask,
            spread: window.spread,
            spread_category,
            opened_at_ns: window.opened_at_ns,
            closed_at_ns,
            duration_ms,
            open_duration_class: open_class,
            close_reason,
            opportunity_class: opp_class,
            observables: obs,
        });

        if let Err(e) = self.window_tx.try_send(event) {
            warn!("window channel full, dropping close event: {e}");
        }
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MarketStore;
    use crate::types::{Category, Market, OpenDurationClass};

    fn make_store_with_market() -> Arc<MarketStore> {
        let store = MarketStore::new();
        store.add_market(Market {
            id: "market1".to_string(),
            question: "Test market".to_string(),
            category: Category::Other,
            end_date_iso: None,
            total_volume: None,
            yes_token_id: "yes1".to_string(),
            no_token_id: "no1".to_string(),
        });
        store
    }

    fn price_msg(asset_id: &str, best_ask: f64) -> PriceChangeMsg {
        PriceChangeMsg {
            asset_id: asset_id.to_string(),
            best_ask,
            best_bid: best_ask - 0.01,
            received_at_ns: now_ns(),
            received_at: Instant::now(),
        }
    }

    #[tokio::test]
    async fn single_tick_does_not_fire_open_event() {
        let store = make_store_with_market();
        let (_price_tx, price_rx) = mpsc::channel(16);
        let (_trade_tx, trade_rx) = mpsc::channel(16);
        let (window_tx, mut window_rx) = mpsc::channel(16);

        let mut detector = SpreadDetector::new(store.clone(), price_rx, trade_rx, window_tx);

        // Seed no-side in detector's local cache
        detector.handle_price_change(price_msg("no1", 0.45)).await;
        // yes=0.45, no=0.45 → spread=0.10 (arb) — opens as pending
        detector.handle_price_change(price_msg("yes1", 0.45)).await;
        // Immediately close on next tick (only 1 arb tick)
        detector.handle_price_change(price_msg("yes1", 0.55)).await;

        // Only a Close event should fire (classified SingleTick), never an Open.
        let event = window_rx.try_recv().expect("expected Close event");
        match event {
            WindowEvent::Close(c) => {
                assert_eq!(c.observables.tick_count, 1);
                assert_eq!(c.open_duration_class, OpenDurationClass::SingleTick);
            }
            WindowEvent::Open(_) => panic!("single-tick window must not fire Open"),
        }
        assert!(window_rx.try_recv().is_err(), "no further events expected");
    }

    #[tokio::test]
    async fn multi_tick_fires_open_then_close() {
        let store = make_store_with_market();
        let (_price_tx, price_rx) = mpsc::channel(16);
        let (_trade_tx, trade_rx) = mpsc::channel(16);
        let (window_tx, mut window_rx) = mpsc::channel(16);

        let mut detector = SpreadDetector::new(store.clone(), price_rx, trade_rx, window_tx);

        // Seed no-side in detector's local cache
        detector.handle_price_change(price_msg("no1", 0.45)).await;

        // Tick 1: spread opens (pending)
        detector.handle_price_change(price_msg("yes1", 0.45)).await;
        // Tick 2: confirms window — should fire Open
        detector.handle_price_change(price_msg("yes1", 0.45)).await;

        let event = window_rx.try_recv().expect("expected Open event");
        assert!(matches!(event, WindowEvent::Open(_)));

        // Close the window
        detector.handle_price_change(price_msg("yes1", 0.56)).await;
        let event = window_rx.try_recv().expect("expected Close event");
        assert!(matches!(event, WindowEvent::Close(_)));
    }
}
