use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::interval;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use crate::config::{RECONNECT_BACKOFF_MS, WS_PING_INTERVAL_SECS, WS_SUBSCRIBE_CHUNK_SIZE};
use crate::error::Result;
use crate::state::market_store::MarketStore;
use crate::types::{ControlMsg, PriceChangeMsg, TradeMsg};
use crate::ws::messages::{ParsedFrame, parse_ws_frame};

/// Manages the single persistent WebSocket connection to Polymarket's CLOB feed.
pub struct WsManager {
    ws_url: String,
    store: Arc<MarketStore>,
    price_tx: mpsc::Sender<PriceChangeMsg>,
    trade_tx: mpsc::Sender<TradeMsg>,
    control_rx: mpsc::Receiver<ControlMsg>,
    /// Total WS frames received since process start (for flow diagnostics).
    frames_received: Arc<AtomicU64>,
    /// Total price events routed to the detector.
    price_msgs_routed: Arc<AtomicU64>,
    /// Per-event-type counters for diagnostics.
    book_snapshots: Arc<AtomicU64>,
    price_changes: Arc<AtomicU64>,
    trade_events: Arc<AtomicU64>,
}

impl WsManager {
    pub fn new(
        ws_url: String,
        store: Arc<MarketStore>,
        price_tx: mpsc::Sender<PriceChangeMsg>,
        trade_tx: mpsc::Sender<TradeMsg>,
        control_rx: mpsc::Receiver<ControlMsg>,
    ) -> Self {
        Self {
            ws_url,
            store,
            price_tx,
            trade_tx,
            control_rx,
            frames_received: Arc::new(AtomicU64::new(0)),
            price_msgs_routed: Arc::new(AtomicU64::new(0)),
            book_snapshots: Arc::new(AtomicU64::new(0)),
            price_changes: Arc::new(AtomicU64::new(0)),
            trade_events: Arc::new(AtomicU64::new(0)),
        }
    }

    pub async fn run(mut self) {
        let mut backoff_idx = 0usize;

        loop {
            info!("WS connecting to {}", self.ws_url);
            match self.connect_once().await {
                Ok(()) => {
                    info!("WS connection closed cleanly");
                    backoff_idx = 0;
                }
                Err(e) => {
                    error!("WS connection error: {e}");
                }
            }

            let delay_ms = RECONNECT_BACKOFF_MS
                .get(backoff_idx)
                .copied()
                .unwrap_or(*RECONNECT_BACKOFF_MS.last().unwrap());
            backoff_idx = (backoff_idx + 1).min(RECONNECT_BACKOFF_MS.len() - 1);

            warn!("WS reconnecting in {delay_ms}ms");
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }

    async fn connect_once(&mut self) -> Result<()> {
        let (ws_stream, _) = connect_async(&self.ws_url).await?;
        let (mut write, mut read) = ws_stream.split();

        // Initial subscription: send in chunks to avoid server-side frame size limits.
        let asset_ids = self.store.all_asset_ids();
        if !asset_ids.is_empty() {
            let chunks: Vec<_> = asset_ids.chunks(WS_SUBSCRIBE_CHUNK_SIZE).collect();
            let total_chunks = chunks.len();
            for (i, chunk) in chunks.into_iter().enumerate() {
                let sub_msg = build_subscribe_msg(chunk);
                write.send(Message::Text(sub_msg.into())).await?;
                if total_chunks > 1 {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                debug!("WS subscribe chunk {}/{} ({} ids)", i + 1, total_chunks, chunk.len());
            }
            info!("WS subscribed to {} asset_ids in {} chunk(s)", asset_ids.len(), total_chunks);
        }

        let mut ping_interval = interval(Duration::from_secs(WS_PING_INTERVAL_SECS));
        ping_interval.tick().await; // consume immediate first tick

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            self.handle_frame(&text).await;
                        }
                        Some(Ok(Message::Ping(data))) => {
                            write.send(Message::Pong(data)).await?;
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            return Ok(());
                        }
                        Some(Err(e)) => return Err(e.into()),
                        Some(Ok(_)) => {}
                    }
                }

                _ = ping_interval.tick() => {
                    debug!("WS ping");
                    write.send(Message::Ping(vec![].into())).await?;
                }

                ctrl = self.control_rx.recv() => {
                    match ctrl {
                        Some(ControlMsg::Subscribe(markets)) => {
                            let new_ids: Vec<String> = markets.iter()
                                .flat_map(|m| [m.yes_token_id.clone(), m.no_token_id.clone()])
                                .collect();
                            let sub_msg = build_subscribe_msg(&new_ids);
                            write.send(Message::Text(sub_msg.into())).await?;
                            info!("WS dynamically subscribed to {} new asset_ids", new_ids.len());
                        }
                        Some(ControlMsg::Unsubscribe(market_id)) => {
                            if let Some(ids) = self.store.token_ids_for_market(&market_id) {
                                let unsub_msg = build_unsubscribe_msg(&ids);
                                write.send(Message::Text(unsub_msg.into())).await?;
                                info!("WS unsubscribed market {market_id}");
                            }
                        }
                        None => {
                            // Control channel dropped — shut down
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    async fn handle_frame(&self, text: &str) {
        let received_at = std::time::Instant::now();
        let received_at_ns = now_ns();

        let total_frames = self.frames_received.fetch_add(1, Ordering::Relaxed) + 1;
        if total_frames % 500 == 0 {
            let price_routed = self.price_msgs_routed.load(Ordering::Relaxed);
            let snaps = self.book_snapshots.load(Ordering::Relaxed);
            let pchg = self.price_changes.load(Ordering::Relaxed);
            let trades = self.trade_events.load(Ordering::Relaxed);
            info!(
                frames = total_frames,
                price_msgs = price_routed,
                snapshots = snaps,
                price_changes = pchg,
                trades = trades,
                "[WS] {total_frames} frames | routed={price_routed} | snap={snaps} pchg={pchg} trade={trades}"
            );
        }

        for event in parse_ws_frame(text) {
            match event {
                ParsedFrame::BookSnapshot { asset_id, asks, bids } => {
                    self.book_snapshots.fetch_add(1, Ordering::Relaxed);
                    // Parse level strings into (price, size) pairs.
                    let parsed_asks: Vec<(f64, f64)> = asks.iter()
                        .filter_map(|l| {
                            let p = l.price.parse::<f64>().ok()?;
                            let s = l.size.parse::<f64>().ok()?;
                            Some((p, s))
                        })
                        .collect();
                    let parsed_bids: Vec<(f64, f64)> = bids.iter()
                        .filter_map(|l| {
                            let p = l.price.parse::<f64>().ok()?;
                            let s = l.size.parse::<f64>().ok()?;
                            Some((p, s))
                        })
                        .collect();

                    if let Some((best_ask, best_bid)) =
                        self.store.apply_book_snapshot(&asset_id, &parsed_asks, &parsed_bids)
                    {
                        debug!(asset_id = %asset_id, best_ask, best_bid, "book snapshot applied");
                        if best_ask > 0.0 {
                            self.route_price_msg(
                                asset_id,
                                best_ask,
                                best_bid,
                                received_at_ns,
                                received_at,
                            );
                        }
                    }
                }

                ParsedFrame::BookPriceChange { asset_id, change, best_bid: server_bid, best_ask: server_ask } => {
                    self.price_changes.fetch_add(1, Ordering::Relaxed);
                    // Apply the individual level change to the local order book,
                    // then use the LOCAL book's computed best prices.
                    // This matches the TS bot approach — the local book is the
                    // source of truth, not server-provided best_ask/best_bid.
                    let (ba, bb) = if let (Ok(p), Ok(s)) = (change.price.parse::<f64>(), change.size.parse::<f64>()) {
                        let is_ask = change.side == "SELL";
                        match self.store.apply_book_changes(&asset_id, &[(p, is_ask, s)]) {
                            Some((a, b)) if a > 0.0 => (a, b),
                            _ => continue,
                        }
                    } else {
                        match self.store.best_prices(&asset_id) {
                            Some((a, b)) if a > 0.0 => (a, b),
                            _ => continue,
                        }
                    };

                    // Log divergence between local book and server-provided prices.
                    if let (Some(sa), Some(sb)) = (server_ask, server_bid) {
                        let ask_diff = (ba - sa).abs();
                        let bid_diff = (bb - sb).abs();
                        if ask_diff > 0.001 || bid_diff > 0.001 {
                            debug!(
                                asset = %asset_id,
                                local_ask = ba,
                                server_ask = sa,
                                local_bid = bb,
                                server_bid = sb,
                                "[PRICE DIVERGENCE] local vs server: ask_diff={ask_diff:.4} bid_diff={bid_diff:.4}"
                            );
                        }
                    }

                    self.route_price_msg(
                        asset_id,
                        ba,
                        bb,
                        received_at_ns,
                        received_at,
                    );
                }

                ParsedFrame::LastTradePrice { asset_id, price } => {
                    self.trade_events.fetch_add(1, Ordering::Relaxed);
                    let trade_msg = TradeMsg {
                        asset_id,
                        price,
                        received_at_ns,
                    };
                    if let Err(e) = self.trade_tx.try_send(trade_msg) {
                        warn!("trade channel full, dropping message: {e}");
                    }
                }
            }
        }
    }

    fn route_price_msg(
        &self,
        asset_id: String,
        best_ask: f64,
        best_bid: f64,
        received_at_ns: u64,
        received_at: std::time::Instant,
    ) {
        let msg = PriceChangeMsg {
            asset_id,
            best_ask,
            best_bid,
            received_at_ns,
            received_at,
        };
        self.price_msgs_routed.fetch_add(1, Ordering::Relaxed);
        if let Err(e) = self.price_tx.try_send(msg) {
            warn!("price channel full, dropping message: {e}");
        }
    }
}

/// Build a market-channel subscription message.
fn build_subscribe_msg(asset_ids: &[String]) -> String {
    serde_json::json!({
        "assets_ids": asset_ids,
        "type": "market"
    })
    .to_string()
}

fn build_unsubscribe_msg(asset_ids: &[String]) -> String {
    serde_json::json!({
        "assets_ids": asset_ids,
        "operation": "unsubscribe"
    })
    .to_string()
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}
