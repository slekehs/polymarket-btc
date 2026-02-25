# Polymarket Arbitrage Scanner

A high-performance arbitrage detection bot for [Polymarket](https://polymarket.com) binary prediction markets. Every market has Yes and No sides priced $0.00–$1.00. When `yes_best_ask + no_best_ask < $1.00`, that's a risk-free arbitrage window. **Phase 1 detects and classifies these windows. No trades are placed.**

Written in Rust for sub-millisecond detection latency. The previous TypeScript scanner had 150–500ms latency; arbitrage windows typically last 200–500ms. Rust brings detection under 2ms — you see windows as they open, not as they close.

---

## Table of Contents

- [Quick Start](#quick-start)
- [Architecture Overview](#architecture-overview)
- [Data Flow](#data-flow)
- [Component Deep Dive](#component-deep-dive)
- [Window Classification](#window-classification)
- [HTTP API](#http-api)
- [Configuration](#configuration)
- [Database Schema](#database-schema)
- [Development](#development)

---

## Quick Start

```bash
# Build
cargo build --release

# Run (uses defaults: DB_PATH=scanner.db, API_PORT=3000)
./target/release/scanner

# With environment overrides
DB_PATH=./data/scanner.db SCANNER_MAX_SUBSCRIPTIONS=300 API_PORT=3000 ./target/release/scanner
```

The scanner starts an HTTP API on port 3000. A companion Next.js dashboard (if present) connects to it for live monitoring.

---

## Architecture Overview

The scanner is a multi-task async application built with Tokio. It uses a **pipeline architecture**: data flows one-way through channels between tasks, ensuring the hot path (price → spread) never blocks on I/O.

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                           POLYMARKET ARBITRAGE SCANNER                           │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│  ┌──────────────┐     REST      ┌──────────────┐                                │
│  │ Gamma API    │◄──────────────│  Fetcher     │  Bootstrap: fetch markets,     │
│  │ /markets     │               │              │  filter by volume/liquidity/    │
│  └──────────────┘               └──────┬──────┘  expiry                        │
│                                         │                                        │
│                                         ▼                                        │
│  ┌──────────────────────────────────────────────────────────────────────────┐  │
│  │                     MARKET STORE (DashMap, shared)                        │  │
│  │  market_id → metadata  |  asset_id → (best_ask, best_bid) | order books   │  │
│  └──────────────────────────────────────────────────────────────────────────┘  │
│                                         ▲                                        │
│                                         │ apply_book_snapshot / apply_book_changes│
│  ┌──────────────┐     WebSocket        │                                        │
│  │ Polymarket   │──────────────────────┼──────┐                                 │
│  │ CLOB WS      │  price_change,        │      │                                 │
│  │ market       │  book, last_trade     │      │ price_tx, trade_tx              │
│  └──────────────┘                       │      │                                 │
│                                         │      ▼                                 │
│                               ┌─────────────────────────┐                        │
│                               │      WsManager          │                        │
│                               │  Parse frames, route     │                        │
│                               │  to detector + store     │                        │
│                               └─────────────────────────┘                        │
│                                         │                                        │
│                                         ▼                                        │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │                    SPREAD DETECTOR (hot path)                           │   │
│  │  • Local price cache (strict message order)                             │   │
│  │  • spread = 1.0 - (yes_ask + no_ask)                                   │   │
│  │  • 2-tick filter: window must survive 2+ consecutive arb ticks         │   │
│  │  • Classify on close: OpenDurationClass + CloseReason                   │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                         │ window_tx                              │
│                                         ▼                                        │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │                    WINDOW CONSUMER                                      │   │
│  │  • Log Open/Close to stdout                                             │   │
│  │  • Broadcast to WebSocket clients (/ws/events)                          │   │
│  │  • Forward to DbWriter                                                 │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                    │                                    │                        │
│                    │                                    ▼                        │
│                    │                    ┌─────────────────────────┐               │
│                    │                    │      DbWriter           │               │
│                    │                    │  Async SQLite writes    │               │
│                    │                    │  (non-blocking)         │               │
│                    │                    └───────────┬────────────┘               │
│                    │                                │                             │
│                    ▼                                ▼                             │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │                    BACKGROUND TASKS                                     │   │
│  │  • MarketScorer (every 60s): aggregate windows → market_stats           │   │
│  │  • MarketRefresher (every 60s): re-fetch Gamma, add/remove markets      │   │
│  │  • PinnedMarketWatcher (every 10s): manage short-timeframe markets     │   │
│  │  • Book audit (one-shot, 20s): compare WS book vs REST                 │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │                    HTTP API (Axum)                                     │   │
│  │  /markets, /windows/*, /stats/*, /health, /ws/events                    │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

---

## Data Flow

### 1. Bootstrap

1. **Fetcher** calls Gamma REST API: `GET /markets?active=true&closed=false&order=volume24hr&ascending=false`
2. Applies filters: min volume, min liquidity, expiry window (e.g. 30min–72h)
3. Inserts markets into **MarketStore** and **SQLite** `markets` table
4. **WsManager** opens WebSocket to Polymarket CLOB, subscribes to all asset_ids in chunks of 500

### 2. Price Feed

1. Polymarket sends `book` (full snapshots) and `price_change` (incremental) messages
2. **WsManager** parses each frame via `ws/messages::parse_ws_frame`
3. For each token: applies changes to **MarketStore** order book (BTreeMap), updates cached `best_ask`/`best_bid`
4. Emits `PriceChangeMsg` (and `TradeMsg` for `last_trade_price`) to detector via `mpsc` channel

### 3. Spread Detection

1. **SpreadDetector** maintains a **local price cache** (HashMap) updated strictly in message order — avoids races with the shared store
2. On each `PriceChangeMsg`: updates local cache, looks up market's yes/no token IDs, reads both sides from local cache
3. `spread = 1.0 - (yes_ask + no_ask)`
4. **State machine:**
   - `(arb, no window)` → create pending `ActiveWindow`, tick_count=1
   - `(arb, window)` → tick_count++; when tick_count >= 2, emit `WindowEvent::Open`
   - `(no arb, window)` → emit `WindowEvent::Close`, classify, remove window

### 4. Classification & Persistence

1. **Classifier** (`detector/classifier.rs`) uses observables: `tick_count`, `trade_event_fired`, `volume_change_ticks`, `price_shifted`
2. Produces `(OpenDurationClass, CloseReason)` → `opportunity_class` (1–4, 0=noise)
3. **Window consumer** broadcasts to WS clients, forwards to **DbWriter**
4. **DbWriter**: on Open → INSERT with `closed_at=NULL`; on Close → UPDATE existing or INSERT (single-tick)

---

## Component Deep Dive

### WsManager (`src/ws/connection.rs`)

- Single persistent WebSocket to `wss://ws-subscriptions-clob.polymarket.com/ws/market`
- Sends `{"assets_ids": [...], "type": "market"}` to subscribe (chunked, 500 IDs per frame)
- Handles `book`, `price_change`, `last_trade_price` events
- **Order book**: applies snapshots and incremental changes to `MarketStore`; uses local computed `best_ask`/`best_bid` (not server-provided when available — matches TS bot)
- Reconnect with exponential backoff: 100, 200, 400, 800 ms
- Ping every 30s
- Handles `ControlMsg::Subscribe` / `Unsubscribe` for dynamic market adds/removals

### MarketStore (`src/state/market_store.rs`)

- **DashMap**-based concurrent maps: `markets`, `token_state`, `token_to_market`, `token_books`, `pinned_ids`
- Per-token **OrderBook**: BTreeMap keyed by `(price * 10_000).round()` for 4-decimal precision; asks ascending (min=best), bids ascending (max=best)
- `apply_book_snapshot` / `apply_book_changes` → update book, write `TokenState { best_ask, best_bid }`
- `get_spread_inputs(asset_id)` → `(market_id, yes_ask, no_ask, yes_bid, no_bid)` when both sides hydrated
- `get_market_for_token(asset_id)` → `(market_id, yes_token_id, no_token_id)` for detector lookups
- Pinned markets: never removed by MarketRefresher; managed by PinnedMarketWatcher

### SpreadDetector (`src/detector/spread.rs`)

- **Local price cache**: `HashMap<asset_id, (best_ask, best_bid)` — ensures strict message order, no store-update race
- `ActiveWindow` tracks: yes_ask, no_ask, spread, opened_at_ns, tick_count, prev_yes_ask/no_ask (for drift), trade_event_fired, volume_change_ticks, price_shift_ticks, pending
- **MIN_ARB_TICKS = 2**: window must survive 2+ consecutive positive-spread ticks before Open fires
- On close: records `detection_latency_us` (WS receive → spread compute) for the closing tick
- Latency recorded to `LatencyStats` (HDR histogram) for every tick

### Classifier (`src/detector/classifier.rs`)

| OpenDurationClass | Condition |
|-------------------|-----------|
| SingleTick | tick_count < 2 |
| MultiTick | tick_count >= 2 |

| CloseReason | Condition (multi_tick only) |
|-------------|-----------------------------|
| VolumeSpikeGradual | trade_event_fired && volume_change_ticks > 1 |
| VolumeSpikeInstant | trade_event_fired && volume_change_ticks == 1 |
| PriceDrift | !trade_event_fired && price_shifted |
| OrderVanished | !trade_event_fired && !price_shifted |

### DbWriter (`src/db/writer.rs`)

- Dedicated task; never blocks detection
- **Open**: INSERT into `windows` with `closed_at=NULL`
- **Close**: UPDATE row where `market_id=? AND opened_at=? AND closed_at IS NULL`; if no row (single-tick), INSERT full row

### MarketScorer (`src/scorer/market_scorer.rs`)

- Runs every 60s
- Aggregates `windows` from last 24h: count, p1/p2 counts, avg duration, avg spread, max spread, noise_ratio
- `compute_score`: P1=2×, P2=1.5× weighted frequency + duration + spread - noise penalty
- Upserts into `market_stats`

### MarketRefresher (`src/market_refresh.rs`)

- Runs every 60s (config: `MARKET_REFRESH_INTERVAL_SECS`)
- Re-fetches from Gamma, compares to current store
- Removes markets no longer qualifying (except pinned); adds new
- Sends `ControlMsg::Unsubscribe` before `store.remove_market` (WS needs token_ids)
- Sends `ControlMsg::Subscribe(to_add)` for new markets

### PinnedMarketWatcher (`src/market_refresh.rs`)

- For `PINNED_SLUGS` (e.g. `btc-updown-5m,btc-updown-15m`)
- Fetches from Gamma (`order=startDate`) every 30s; parses slug end timestamp
- Only subscribes the *current* market per prefix (smallest end_ts in future)
- Pre-subscribes next market 30s before current expires
- Unsubscribes and removes after 60s grace past expiry

---

## Window Classification

| Priority | Label | OpenDurationClass | CloseReason | Phase 2 Target |
|----------|-------|-------------------|-------------|----------------|
| 1 | P1 — best | MultiTick | VolumeSpikeGradual | Yes |
| 2 | P2 — good | MultiTick | PriceDrift | Yes |
| 3 | P3 — fast required | MultiTick | VolumeSpikeInstant | Harder |
| 4 | P4 — low | MultiTick | OrderVanished | Rarely |
| 0 | Noise | SingleTick | — | No |

**Spread categories** (from `config::spread_thresholds`): &lt;$0.02 noise, $0.02–$0.05 small, $0.05–$0.10 medium, &gt;$0.10 large.

---

## HTTP API

| Endpoint | Description |
|----------|-------------|
| `GET /markets` | All markets with stats; optional `?category=`, `?min_score=` |
| `GET /markets/:id/windows` | Windows for a market; `?limit=`, `?since=` |
| `GET /windows/recent` | Recent windows; `?min_spread=`, `?limit=` |
| `GET /windows/open` | Currently open windows (`closed_at IS NULL`) |
| `GET /stats/summary` | Total markets, windows today, top 10 markets |
| `GET /stats/latency` | p50/p95/p99 detection latency (ms), sample count |
| `GET /health` | ws_connected, markets_subscribed, hydrated_markets, last_window_at_ns, write_queue_pending, detection_p99_us |
| `GET /ws/events` | WebSocket upgrade; JSON `WindowEvent` (Open/Close) frames |

---

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `DB_PATH` | `scanner.db` | SQLite database path |
| `API_PORT` | `3000` | HTTP API port |
| `LOG_LEVEL` | `info` | `tracing` level (debug, info, warn, error) |
| `WS_URL` | Polymarket CLOB WS | Override WebSocket URL |
| `GAMMA_API_URL` | Polymarket Gamma | Override Gamma API base |
| `SCANNER_MAX_SUBSCRIPTIONS` | 200 | Max markets to subscribe |
| `SCANNER_MIN_VOLUME_24H` | 10000 | Min 24h volume (USD) |
| `SCANNER_MIN_LIQUIDITY` | 1000 | Min liquidity (USD) |
| `SCANNER_MIN_EXPIRY_MINUTES` | 30 | Exclude markets expiring sooner |
| `SCANNER_MAX_EXPIRY_HOURS` | 72 | Exclude markets expiring later |
| `PINNED_SLUGS` | (empty) | Comma-separated slug prefixes to always track (e.g. `btc-updown-5m,btc-updown-15m`) |

---

## Database Schema

**markets** — one row per market (from Gamma + refresher)

**windows** — one row per detected window
- `opened_at`, `closed_at` (NULL if still open)
- `yes_ask`, `no_ask`, `combined_cost`, `spread_size`, `spread_category`
- `open_duration_class`, `close_reason`, `opportunity_class`
- `tick_count`, `volume_changed`, `volume_change_ticks`, `price_shifted`
- `detection_latency_us`

**market_stats** — rolling 24h stats per market
- `windows_24h`, `p1_windows_24h`, `p2_windows_24h`
- `avg_window_duration_ms`, `avg_spread_size`, `max_spread_size`
- `noise_ratio`, `opportunity_score`

---

## Development

```bash
# Run tests
cargo test

# TUI (optional)
cargo run --bin tui
```

### Polymarket Docs

- [CLOB WebSocket market channel](https://docs.polymarket.com/developers/CLOB/websocket/market-channel.md)
- [WSS overview](https://docs.polymarket.com/developers/CLOB/websocket/wss-overview.md)
- [Rate limits](https://docs.polymarket.com/quickstart/introduction/rate-limits.md)

### Phase Plan

- **Phase 1A**: Core scanner, WS, detector, latency instrumentation ✓
- **Phase 1B**: SQLite persistence, scorer ✓
- **Phase 1C**: HTTP API, dashboard, VPS deployment ✓
- **Phase 2**: Execution bot (after 7+ days of Phase 1 data)
