use std::collections::BTreeMap;
use std::sync::Arc;

use dashmap::{DashMap, DashSet};

use crate::types::Market;

// ---------------------------------------------------------------------------
// OrderBook
// ---------------------------------------------------------------------------

/// Per-token order book. Prices are stored as integer keys: `(price * 10_000).round() as u32`.
/// This avoids floating-point map keys while supporting 4 decimal places of precision.
///
/// For asks, `BTreeMap::keys().next()` is O(log n) and gives the minimum (best ask).
/// For bids, `BTreeMap::keys().next_back()` gives the maximum (best bid).
#[derive(Debug, Default)]
struct OrderBook {
    /// price_key → size. Sorted ascending; minimum key = best ask.
    asks: BTreeMap<u32, f64>,
    /// price_key → size. Sorted ascending; maximum key = best bid.
    bids: BTreeMap<u32, f64>,
}

impl OrderBook {
    #[inline]
    fn price_key(price: f64) -> u32 {
        (price * 10_000.0).round() as u32
    }

    #[inline]
    fn key_to_price(key: u32) -> f64 {
        key as f64 / 10_000.0
    }

    fn apply_snapshot(&mut self, asks: &[(f64, f64)], bids: &[(f64, f64)]) {
        self.asks.clear();
        for &(price, size) in asks {
            if size > 0.0 {
                self.asks.insert(Self::price_key(price), size);
            }
        }
        self.bids.clear();
        for &(price, size) in bids {
            if size > 0.0 {
                self.bids.insert(Self::price_key(price), size);
            }
        }
    }

    /// `is_ask`: true = SELL side (ask), false = BUY side (bid).
    fn apply_change(&mut self, price: f64, is_ask: bool, size: f64) {
        let key = Self::price_key(price);
        let map = if is_ask { &mut self.asks } else { &mut self.bids };
        if size == 0.0 {
            map.remove(&key);
        } else {
            map.insert(key, size);
        }
    }

    /// Minimum ask price — O(log n).
    fn best_ask(&self) -> Option<f64> {
        self.asks.keys().next().map(|&k| Self::key_to_price(k))
    }

    /// Maximum bid price — O(log n).
    fn best_bid(&self) -> Option<f64> {
        self.bids.keys().next_back().map(|&k| Self::key_to_price(k))
    }
}

// ---------------------------------------------------------------------------
// TokenState — cached best prices for O(1) hot-path reads by the detector
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TokenState {
    pub best_ask: f64,
    pub best_bid: f64,
}

/// Maps token_id → Market for fast reverse lookup (asset_id → market).
#[derive(Debug, Clone)]
pub struct TokenMarketRef {
    pub market_id: String,
    pub is_yes: bool,
}

// ---------------------------------------------------------------------------
// MarketStore
// ---------------------------------------------------------------------------

pub struct MarketStore {
    /// market_id → Market metadata
    markets: DashMap<String, Market>,
    /// asset_id → cached (best_ask, best_bid) for the detector hot path
    token_state: DashMap<String, TokenState>,
    /// asset_id → (market_id, is_yes)
    token_to_market: DashMap<String, TokenMarketRef>,
    /// asset_id → live order book (maintained from WS Book subscription)
    token_books: DashMap<String, OrderBook>,
    /// market_ids that are pinned — never removed by the regular refresh cycle
    pinned_ids: DashSet<String>,
}

impl MarketStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            markets: DashMap::new(),
            token_state: DashMap::new(),
            token_to_market: DashMap::new(),
            token_books: DashMap::new(),
            pinned_ids: DashSet::new(),
        })
    }

    /// Mark a market as pinned so the regular refresh cycle never removes it.
    pub fn pin_market(&self, market_id: &str) {
        self.pinned_ids.insert(market_id.to_string());
    }

    pub fn is_pinned(&self, market_id: &str) -> bool {
        self.pinned_ids.contains(market_id)
    }

    pub fn pinned_ids(&self) -> Vec<String> {
        self.pinned_ids.iter().map(|r| r.key().clone()).collect()
    }

    pub fn add_market(&self, market: Market) {
        self.token_to_market.insert(
            market.yes_token_id.clone(),
            TokenMarketRef { market_id: market.id.clone(), is_yes: true },
        );
        self.token_to_market.insert(
            market.no_token_id.clone(),
            TokenMarketRef { market_id: market.id.clone(), is_yes: false },
        );
        self.token_books.entry(market.yes_token_id.clone()).or_default();
        self.token_books.entry(market.no_token_id.clone()).or_default();
        self.markets.insert(market.id.clone(), market);
    }

    pub fn markets_contains(&self, market_id: &str) -> bool {
        self.markets.contains_key(market_id)
    }

    pub fn remove_market(&self, market_id: &str) {
        if let Some((_, market)) = self.markets.remove(market_id) {
            self.token_to_market.remove(&market.yes_token_id);
            self.token_to_market.remove(&market.no_token_id);
            self.token_state.remove(&market.yes_token_id);
            self.token_state.remove(&market.no_token_id);
            self.token_books.remove(&market.yes_token_id);
            self.token_books.remove(&market.no_token_id);
        }
    }

    /// Apply a full book snapshot for a token and update the cached best prices.
    ///
    /// `asks`/`bids` are `(price, size)` pairs — size=0 levels are skipped.
    /// Returns `(best_ask, best_bid)` if the snapshot produced usable prices, else None.
    pub fn apply_book_snapshot(
        &self,
        asset_id: &str,
        asks: &[(f64, f64)],
        bids: &[(f64, f64)],
    ) -> Option<(f64, f64)> {
        if !self.token_to_market.contains_key(asset_id) {
            return None;
        }
        let mut book = self.token_books.entry(asset_id.to_string()).or_default();
        book.apply_snapshot(asks, bids);
        let best_ask = book.best_ask().unwrap_or(0.0);
        let best_bid = book.best_bid().unwrap_or(0.0);
        drop(book);

        if best_ask > 0.0 || best_bid > 0.0 {
            self.token_state.insert(
                asset_id.to_string(),
                TokenState { best_ask, best_bid },
            );
            Some((best_ask, best_bid))
        } else {
            None
        }
    }

    /// Apply incremental order-level changes for a token and update the cached best prices.
    ///
    /// `changes` are `(price, is_ask, size)` — `is_ask=true` means SELL side, false means BUY.
    /// Returns `(best_ask, best_bid)` after applying all changes.
    pub fn apply_book_changes(
        &self,
        asset_id: &str,
        changes: &[(f64, bool, f64)],
    ) -> Option<(f64, f64)> {
        if !self.token_to_market.contains_key(asset_id) {
            return None;
        }
        let mut book = self.token_books.entry(asset_id.to_string()).or_default();
        for &(price, is_ask, size) in changes {
            book.apply_change(price, is_ask, size);
        }
        let best_ask = book.best_ask().unwrap_or(0.0);
        let best_bid = book.best_bid().unwrap_or(0.0);
        drop(book);

        // Only update cached state if we have a real ask price.
        // best_ask=0 means the ask side is empty — don't poison the cache.
        if best_ask > 0.0 || best_bid > 0.0 {
            self.token_state.insert(
                asset_id.to_string(),
                TokenState { best_ask, best_bid },
            );
        }
        Some((best_ask, best_bid))
    }

    /// Directly update cached prices without touching the order book.
    pub fn update_token_price(&self, asset_id: &str, best_ask: f64, best_bid: f64) {
        self.token_state.insert(
            asset_id.to_string(),
            TokenState { best_ask, best_bid },
        );
    }

    /// Read current cached best prices for a token. Returns `(best_ask, best_bid)`.
    pub fn best_prices(&self, asset_id: &str) -> Option<(f64, f64)> {
        let ts = self.token_state.get(asset_id)?;
        Some((ts.best_ask, ts.best_bid))
    }

    /// Returns spread inputs for the market that owns `asset_id`:
    /// `(market_id, yes_ask, no_ask, yes_bid, no_bid)`.
    /// Returns None if either side is missing or has no real ask.
    pub fn get_spread_inputs(&self, asset_id: &str) -> Option<(String, f64, f64, f64, f64)> {
        let token_ref = self.token_to_market.get(asset_id)?;
        let market_id = token_ref.market_id.clone();
        drop(token_ref);

        let market = self.markets.get(&market_id)?;
        let yes_state = self.token_state.get(&market.yes_token_id)?;
        let no_state = self.token_state.get(&market.no_token_id)?;

        if yes_state.best_ask <= 0.0 || no_state.best_ask <= 0.0 {
            return None;
        }

        Some((market_id, yes_state.best_ask, no_state.best_ask, yes_state.best_bid, no_state.best_bid))
    }

    /// Returns (market_id, yes_token_id, no_token_id) for the market that owns `asset_id`,
    /// without reading any prices. Used by the detector's local price cache.
    pub fn get_market_for_token(&self, asset_id: &str) -> Option<(String, String, String)> {
        let token_ref = self.token_to_market.get(asset_id)?;
        let market_id = token_ref.market_id.clone();
        drop(token_ref);
        let market = self.markets.get(&market_id)?;
        Some((market_id, market.yes_token_id.clone(), market.no_token_id.clone()))
    }

    pub fn get_market(&self, market_id: &str) -> Option<Market> {
        self.markets.get(market_id).map(|m| m.clone())
    }

    pub fn market_count(&self) -> usize {
        self.markets.len()
    }

    /// Count of markets where both yes and no token prices have been received.
    pub fn hydrated_market_count(&self) -> usize {
        self.markets
            .iter()
            .filter(|entry| {
                let m = entry.value();
                self.token_state.contains_key(&m.yes_token_id)
                    && self.token_state.contains_key(&m.no_token_id)
            })
            .count()
    }

    pub fn all_asset_ids(&self) -> Vec<String> {
        self.token_to_market.iter().map(|e| e.key().clone()).collect()
    }

    /// Returns `[yes_token_id, no_token_id]` for a market, used for unsubscription.
    pub fn token_ids_for_market(&self, market_id: &str) -> Option<Vec<String>> {
        let market = self.markets.get(market_id)?;
        Some(vec![market.yes_token_id.clone(), market.no_token_id.clone()])
    }

    pub fn add_markets(&self, markets: Vec<Market>) {
        for market in markets {
            self.add_market(market);
        }
    }

    pub fn all_market_ids(&self) -> Vec<String> {
        self.markets.iter().map(|e| e.key().clone()).collect()
    }
}

impl Default for MarketStore {
    fn default() -> Self {
        Self {
            markets: DashMap::new(),
            token_state: DashMap::new(),
            token_to_market: DashMap::new(),
            token_books: DashMap::new(),
            pinned_ids: DashSet::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Category, Market};

    fn test_market() -> Market {
        Market {
            id: "market1".to_string(),
            question: "Test".to_string(),
            category: Category::Other,
            end_date_iso: None,
            total_volume: None,
            yes_token_id: "yes1".to_string(),
            no_token_id: "no1".to_string(),
        }
    }

    #[test]
    fn snapshot_sets_best_ask_and_bid() {
        let store = MarketStore::new();
        store.add_market(test_market());

        let result = store.apply_book_snapshot(
            "yes1",
            &[(0.55, 100.0), (0.60, 50.0)],
            &[(0.54, 200.0), (0.50, 75.0)],
        );
        assert!(result.is_some());
        let (best_ask, best_bid) = result.unwrap();
        assert!((best_ask - 0.55).abs() < 1e-6, "best_ask={best_ask}");
        assert!((best_bid - 0.54).abs() < 1e-6, "best_bid={best_bid}");
    }

    #[test]
    fn price_change_removes_level_and_updates_best_ask() {
        let store = MarketStore::new();
        store.add_market(test_market());

        // Seed book: asks at 0.55 and 0.60
        store.apply_book_snapshot("yes1", &[(0.55, 100.0), (0.60, 50.0)], &[]);

        // Remove the best ask (size=0 means cancelled)
        let result = store.apply_book_changes("yes1", &[(0.55, true, 0.0)]);
        assert!(result.is_some());
        let (best_ask, _) = result.unwrap();
        assert!((best_ask - 0.60).abs() < 1e-6, "best_ask should have moved to 0.60, got {best_ask}");
    }

    #[test]
    fn unknown_token_returns_none() {
        let store = MarketStore::new();
        store.add_market(test_market());

        let result = store.apply_book_snapshot("unknown_token", &[(0.55, 100.0)], &[]);
        assert!(result.is_none());
    }

    #[test]
    fn get_spread_inputs_requires_both_sides() {
        let store = MarketStore::new();
        store.add_market(test_market());

        // Only one side populated — should return None
        store.apply_book_snapshot("yes1", &[(0.55, 100.0)], &[]);
        assert!(store.get_spread_inputs("yes1").is_none());

        // Both sides populated — should return Some
        store.apply_book_snapshot("no1", &[(0.46, 100.0)], &[]);
        let result = store.get_spread_inputs("yes1");
        assert!(result.is_some());
        let (_, yes_ask, no_ask, _, _) = result.unwrap();
        assert!((yes_ask - 0.55).abs() < 1e-6);
        assert!((no_ask - 0.46).abs() < 1e-6);
    }
}
