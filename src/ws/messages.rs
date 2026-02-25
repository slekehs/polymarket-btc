use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;
use tracing::warn;

static PARSE_FAILURES: AtomicU64 = AtomicU64::new(0);

/// A single price level in a book snapshot.
#[derive(Debug, Deserialize, Clone)]
pub struct BookLevel {
    pub price: String,
    pub size: String,
}

/// A single order-level change in a price_change message.
#[derive(Debug, Deserialize, Clone)]
pub struct BookChange {
    pub price: String,
    /// "SELL" = ask side, "BUY" = bid side.
    pub side: String,
    pub size: String,
}

/// One entry inside the `price_changes` array (September 2025+ format).
#[derive(Debug, Deserialize, Clone)]
pub struct PriceChangeEntry {
    pub asset_id: String,
    pub price: String,
    pub size: String,
    pub side: String,
    pub best_bid: Option<String>,
    pub best_ask: Option<String>,
}

/// Raw deserializable shape covering all market-channel WS messages.
/// Fields are optional because different event types carry different subsets.
#[derive(Debug, Deserialize)]
struct RawBookMsg {
    pub event_type: Option<String>,
    /// Present on `book` and `last_trade_price`; absent on new `price_change` format.
    pub asset_id: Option<String>,
    pub asks: Option<Vec<BookLevel>>,
    pub bids: Option<Vec<BookLevel>>,
    /// New `price_change` format (September 2025+): array of per-asset change entries.
    pub price_changes: Option<Vec<PriceChangeEntry>>,
    /// `last_trade_price` only.
    pub price: Option<String>,
}

/// Parsed event from a single WS message object.
#[derive(Debug)]
pub enum ParsedFrame {
    /// Full order book snapshot for one token.
    BookSnapshot {
        asset_id: String,
        asks: Vec<BookLevel>,
        bids: Vec<BookLevel>,
    },
    /// Incremental order-level change for one token.
    /// `best_bid`/`best_ask` are provided directly by the server when available.
    BookPriceChange {
        asset_id: String,
        change: BookChange,
        best_bid: Option<f64>,
        best_ask: Option<f64>,
    },
    /// A trade executed; used for volume spike classification.
    LastTradePrice {
        asset_id: String,
        price: f64,
    },
}

/// Parse a raw WebSocket text frame into zero or more events.
///
/// Polymarket market-channel messages arrive as either:
/// - A single JSON object (book snapshots, last_trade_price, or price_change)
/// - An array of JSON objects
///
/// The `price_change` format (September 2025+) nests per-asset data inside a
/// `price_changes` array, each entry carrying `asset_id`, the changed level,
/// and the resulting `best_bid`/`best_ask`.
pub fn parse_ws_frame(raw: &str) -> Vec<ParsedFrame> {
    let msgs: Vec<RawBookMsg> = if raw.trim_start().starts_with('[') {
        serde_json::from_str(raw).unwrap_or_default()
    } else {
        match serde_json::from_str::<RawBookMsg>(raw) {
            Ok(m) => vec![m],
            Err(_) => vec![],
        }
    };

    if msgs.is_empty() {
        let count = PARSE_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
        if count <= 10 || count % 1000 == 0 {
            let sample = &raw[..500.min(raw.len())];
            warn!(count, "[WS PARSE] unrecognized frame: {sample}");
        }
        return vec![];
    }

    let mut frames = Vec::new();
    for msg in msgs {
        expand_raw_msg(msg, &mut frames);
    }
    frames
}

/// Expands one raw message into zero or more `ParsedFrame`s.
/// `price_change` messages can contain multiple entries (one per asset) so a
/// single raw message may produce multiple frames.
fn expand_raw_msg(msg: RawBookMsg, out: &mut Vec<ParsedFrame>) {
    match msg.event_type.as_deref() {
        Some("book") => {
            if let Some(asset_id) = msg.asset_id {
                out.push(ParsedFrame::BookSnapshot {
                    asset_id,
                    asks: msg.asks.unwrap_or_default(),
                    bids: msg.bids.unwrap_or_default(),
                });
            }
        }
        Some("price_change") => {
            let entries = match msg.price_changes {
                Some(e) if !e.is_empty() => e,
                _ => return,
            };
            for entry in entries {
                let best_bid = entry.best_bid.as_deref().and_then(|s| s.parse::<f64>().ok());
                let best_ask = entry.best_ask.as_deref().and_then(|s| s.parse::<f64>().ok());
                let change = BookChange {
                    price: entry.price,
                    side: entry.side,
                    size: entry.size,
                };
                out.push(ParsedFrame::BookPriceChange {
                    asset_id: entry.asset_id,
                    change,
                    best_bid,
                    best_ask,
                });
            }
        }
        Some("last_trade_price") => {
            if let (Some(asset_id), Some(price_str)) = (msg.asset_id, msg.price.as_deref()) {
                if let Ok(price) = price_str.parse::<f64>() {
                    out.push(ParsedFrame::LastTradePrice { asset_id, price });
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_book_snapshot_single_object() {
        let raw = r#"{"event_type":"book","asset_id":"tok1","asks":[{"price":"0.55","size":"100"}],"bids":[{"price":"0.54","size":"200"}]}"#;
        let frames = parse_ws_frame(raw);
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            ParsedFrame::BookSnapshot { asset_id, asks, bids } => {
                assert_eq!(asset_id, "tok1");
                assert_eq!(asks.len(), 1);
                assert_eq!(asks[0].price, "0.55");
                assert_eq!(bids.len(), 1);
                assert_eq!(bids[0].price, "0.54");
            }
            other => panic!("expected BookSnapshot, got {other:?}"),
        }
    }

    #[test]
    fn parses_new_price_change_format() {
        let raw = r#"{"event_type":"price_change","market":"0xabc","timestamp":"1757908892351","price_changes":[{"asset_id":"tok1","price":"0.55","size":"200","side":"SELL","best_bid":"0.52","best_ask":"0.55"}]}"#;
        let frames = parse_ws_frame(raw);
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            ParsedFrame::BookPriceChange { asset_id, change, best_bid, best_ask } => {
                assert_eq!(asset_id, "tok1");
                assert_eq!(change.side, "SELL");
                assert_eq!(change.price, "0.55");
                assert_eq!(change.size, "200");
                assert!((best_bid.unwrap() - 0.52).abs() < 1e-9);
                assert!((best_ask.unwrap() - 0.55).abs() < 1e-9);
            }
            other => panic!("expected BookPriceChange, got {other:?}"),
        }
    }

    #[test]
    fn price_change_multiple_entries() {
        let raw = r#"{"event_type":"price_change","market":"0xabc","price_changes":[{"asset_id":"tok1","price":"0.55","size":"0","side":"SELL","best_bid":"0.52","best_ask":"0.56"},{"asset_id":"tok2","price":"0.45","size":"50","side":"BUY","best_bid":"0.45","best_ask":"0.47"}]}"#;
        let frames = parse_ws_frame(raw);
        assert_eq!(frames.len(), 2);
        match &frames[0] {
            ParsedFrame::BookPriceChange { asset_id, change, .. } => {
                assert_eq!(asset_id, "tok1");
                assert_eq!(change.side, "SELL");
                assert_eq!(change.size, "0");
            }
            other => panic!("expected BookPriceChange, got {other:?}"),
        }
        match &frames[1] {
            ParsedFrame::BookPriceChange { asset_id, change, .. } => {
                assert_eq!(asset_id, "tok2");
                assert_eq!(change.side, "BUY");
            }
            other => panic!("expected BookPriceChange, got {other:?}"),
        }
    }

    #[test]
    fn empty_price_changes_skipped() {
        let raw = r#"{"event_type":"price_change","market":"0xabc","price_changes":[]}"#;
        let frames = parse_ws_frame(raw);
        assert!(frames.is_empty());
    }

    #[test]
    fn parses_last_trade_price() {
        let raw = r#"{"event_type":"last_trade_price","asset_id":"tok1","price":"0.57"}"#;
        let frames = parse_ws_frame(raw);
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            ParsedFrame::LastTradePrice { asset_id, price } => {
                assert_eq!(asset_id, "tok1");
                assert!((price - 0.57).abs() < 1e-9);
            }
            other => panic!("expected LastTradePrice, got {other:?}"),
        }
    }

    #[test]
    fn unknown_event_type_returns_empty() {
        let raw = r#"{"event_type":"some_other_event","asset_id":"tok1"}"#;
        let frames = parse_ws_frame(raw);
        assert!(frames.is_empty());
    }

    #[test]
    fn garbage_returns_empty() {
        let raw = r#"{"totally":"unrelated"}"#;
        let frames = parse_ws_frame(raw);
        assert!(frames.is_empty());
    }
}
