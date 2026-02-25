# Polymarket Scanner — Improvements Guide

This document explains the improvements implemented for better data review, where you'll see them, and how they work.

---

## 1. Latency Instrumentation (`/stats/latency`)

### What It Is
Detection latency tracking from the moment a price message arrives from the WebSocket until the spread is computed in the detector.

### Where You'll See It
- **Endpoint:** `GET /stats/latency`
- **Response:**
  ```json
  {
    "p50_ms": 0.12,
    "p95_ms": 0.45,
    "p99_ms": 0.89,
    "sample_count": 12345
  }
  ```

### How It Works
1. `WsManager` sets `received_at: Instant` when each `PriceChangeMsg` is created.
2. `SpreadDetector` records `msg.received_at.elapsed()` at the spread computation point in `handle_price_change`.
3. Values are stored in an in-memory HDR histogram (`src/api/latency.rs`).
4. The API reads p50/p95/p99 percentiles in microseconds and returns them as milliseconds.

### Dashboard Use
Poll this endpoint (e.g., every 5s) to show a Latency tab with p50/p95/p99. Target: p99 < 5ms for Phase 2 readiness.

---

## 2. Health Endpoint (`/health`)

### What It Is
Operational health metrics exposed for the dashboard Health tab.

### Where You'll See It
- **Endpoint:** `GET /health`
- **Response:**
  ```json
  {
    "ws_connected": true,
    "markets_subscribed": 156,
    "hydrated_markets": 142,
    "total_markets": 156,
    "last_window_at_ns": 1730000000123456789,
    "write_queue_pending": 2,
    "detection_p99_us": 890
  }
  ```

### How It Works
- **`ws_connected`:** Set by `WsManager` when the WebSocket connects/disconnects.
- **`markets_subscribed` / `total_markets`:** From `MarketStore.market_count()`.
- **`hydrated_markets`:** Markets with both YES and NO token prices received from the feed.
- **`last_window_at_ns`:** Nanosecond timestamp of the most recent window close.
- **`write_queue_pending`:** Count of window close events sent to the DB writer but not yet written. Incremented when a close is sent, decremented when the writer processes it.
- **`detection_p99_us`:** From the latency histogram (see §1).

### Dashboard Use
Use for health checks: green if `ws_connected`, low `write_queue_pending`, recent `last_window_at_ns`, and `detection_p99_us` < 5000.

---

## 3. WebSocket Event Stream (`/ws/events`)

### What It Is
Real-time push of window open/close events to dashboard clients.

### Where You'll See It
- **Endpoint:** `GET /ws/events` (WebSocket upgrade)
- **Protocol:** JSON text frames, one `WindowEvent` per message.

### How It Works
1. `window_consumer` broadcasts every `WindowEvent` (Open and Close) to a `tokio::sync::broadcast` channel.
2. When a client connects to `/ws/events`, the handler subscribes to that channel.
3. Each event is serialized to JSON and sent as a WebSocket text frame.

**Example frame:**
```json
{"Open":{"market_id":"0x123...","yes_ask":0.45,"no_ask":0.46,"spread":0.09,"spread_category":"small","opened_at_ns":1730000000123456789}}
```

```json
{"Close":{"market_id":"0x123...","yes_ask":0.46,"no_ask":0.47,"spread":0.0,"closed_at_ns":1730000000456789012,"duration_ms":333.3,"open_duration_class":"multi_tick","close_reason":"volume_spike_gradual","opportunity_class":1,"observables":{"tick_count":5,"trade_event_fired":true,"volume_change_ticks":3,"price_shifted":true},"detection_latency_us":120}}
```

### Dashboard Use
Connect to `wss://your-scanner:3000/ws/events` for the Live Feed tab instead of polling. Render incoming JSON as event rows.

---

## 4. Detection Latency Per Window (`detection_latency_us`)

### What It Is
Per-window microsecond latency for the closing tick (WS receive → spread computation).

### Where You'll See It
- **Database:** `windows.detection_latency_us` (new column)
- **API responses:** `GET /markets/:id/windows`, `GET /windows/recent`, `GET /windows/open` include `detection_latency_us` in each window object.

**Example:**
```json
{
  "id": 42,
  "market_id": "0x123...",
  "opened_at": 1730000000123456789,
  "closed_at": 1730000000456789012,
  "duration_ms": 333.3,
  "spread_size": 0.09,
  "detection_latency_us": 120,
  ...
}
```

### How It Works
1. When the detector closes a window, it computes `detect_elapsed = msg.received_at.elapsed()` for the closing tick.
2. That value (in microseconds) is passed into `WindowCloseEvent.detection_latency_us`.
3. `DbWriter` stores it in the `windows` table.
4. API queries select and return it.

### Use
Analyze latency by market, time, or spread size; spot slow bursts or outliers.

---

## 5. P1/P2 Weighting in Scorer

### What It Is
Market scoring that favors P1 (VolumeSpikeGradual) and P2 (PriceDrift) windows over noise and lower-priority closes.

### Where You'll See It
- **Database:** `market_stats.p1_windows_24h`, `market_stats.p2_windows_24h`
- **Behavior:** Markets with more P1/P2 windows rank higher via `opportunity_score`.

### How It Works
1. The scorer query counts windows by `opportunity_class`:
   - `p1_windows_24h` = `opportunity_class = 1` (VolumeSpikeGradual)
   - `p2_windows_24h` = `opportunity_class = 2` (PriceDrift)
2. `compute_score` uses a weighted frequency:
   - P1 windows count 2×
   - P2 windows count 1.5×
   - Other windows count 1×
3. The weighted count is capped and normalized into the frequency score component.

### Use
Top markets in `GET /markets` and `GET /stats/summary` now emphasize executable opportunities over noise.

---

## 6. Persist Open Events (`/windows/open`)

### What It Is
Both window open and close events are stored. Open events create rows with `closed_at = NULL`; close events update them or insert (for single-tick).

### Where You'll See It
- **Endpoint:** `GET /windows/open` — returns currently open windows.
- **Database:** `windows` rows with `closed_at IS NULL` represent open windows.

**Example response:**
```json
[
  {
    "id": 43,
    "market_id": "0xabc...",
    "opened_at": 1730000000500000000,
    "closed_at": null,
    "duration_ms": null,
    "spread_size": 0.07,
    ...
  }
]
```

### How It Works
1. **On Open:** `DbWriter.write_window_open` inserts a row with `closed_at = NULL`, `duration_ms = NULL`, and classification fields null.
2. **On Close:** `DbWriter.write_window_close` first tries to `UPDATE` the matching row (same `market_id` and `opened_at`, `closed_at IS NULL`).
3. If no row is updated (e.g. single-tick, no prior Open), it `INSERT`s a full row.
4. `GET /windows/open` selects all rows where `closed_at IS NULL`.

### Use
- See current open windows in real time.
- Compute open→close lag and timing without parsing logs.

---

## Summary Table

| Improvement           | Endpoint / Location      | Main Benefit                          |
|----------------------|--------------------------|---------------------------------------|
| Latency histogram    | `GET /stats/latency`     | Latency dashboard, Phase 2 readiness  |
| Health endpoint      | `GET /health`            | Operational status for Health tab     |
| WebSocket events     | `GET /ws/events`         | Live Feed without polling             |
| Per-window latency   | `windows.detection_latency_us`, API | Per-window latency analysis      |
| P1/P2 weighting      | `market_stats`, scorer   | Better market ranking                 |
| Persist open events  | `GET /windows/open`, DB  | Query open windows, timing analysis   |
