# Polymarket Scanner TUI

A terminal user interface for monitoring the Polymarket arbitrage scanner in real time. The TUI connects to the scanner's HTTP API and displays markets, window events, and operational health in a compact, keyboard-driven layout.

**Requires the scanner to be running.** The TUI is a read-only client — it never writes data or controls the scanner.

---

## Quick Start

```bash
# Start the scanner first (in another terminal)
./target/release/scanner

# Run the TUI (default API: http://localhost:3000)
cargo run --bin tui

# Point to a remote scanner
API_URL=http://192.168.1.10:3000 cargo run --bin tui
```

---

## Layout

```
┌─────────────────────────────────────────────────────────────────────────────────────┐
│ Polymarket Scanner  ● connected  │  WS ✓  │  142/156 hydrated  │  42 windows today  │
│ │  45ms avg  │  p99 0.89ms  │  queue 0  │  156 markets                             │
├─────────────────────────────────┬───────────────────────────────────────────────────┤
│  TOP MARKETS                    │  OPEN NOW (2)                                     │
│  #  Market             Score  W  P1 P2 │  Time     Market              Spread       │
│  1  Will BTC hit...    0.82   12  3  2 │  14:32:01 Will BTC hit...     +0.045       │
│  2  ETH above...      0.71    8   1  4 │  14:31:58 ETH above...        +0.032       │
│  3  ...                          │                                                 │
│                                 │  RECENT WINDOWS (50) / market detail              │
│                                 │  Time     Market      Spread  Dur    Class  μs    │
│                                 │  14:32:00 Will BTC... +0.04  120ms  P1     89    │
│                                 │  14:31:55 ETH...      +0.03  45ms   P2     120   │
│                                 │  ...                                              │
├─────────────────────────────────┴───────────────────────────────────────────────────┤
│ [q] quit  [r] refresh  [Tab] switch pane  [↑↓/jk] scroll  [Enter] market windows  [Esc] back │
│ auto-refresh: 2s                                                                     │
└─────────────────────────────────────────────────────────────────────────────────────┘
```

---

## Panes

### Top Markets (left, 40%)

- **#** — Row index
- **Market** — Question text (truncated)
- **Score** — `opportunity_score` from `market_stats` (color: green ≥0.7, yellow ≥0.4, red &lt;0.4)
- **W/24h** — Windows in last 24 hours
- **P1** — Count of P1 (VolumeSpikeGradual) windows
- **P2** — Count of P2 (PriceDrift) windows

### Open Now (right top, when any open)

- Shows currently open arbitrage windows (`GET /windows/open`)
- **Time** — HH:MM:SS when window opened
- **Market** — Question or market ID
- **Spread** — Size of spread (green)

### Recent Windows / Market Windows (right)

- **Recent** (default): `GET /windows/recent` — last 100 closed windows across all markets
- **Market detail** (after Enter on a market): `GET /markets/:id/windows` — windows for that market only

Columns:

- **Time** — Opening time (HH:MM:SS)
- **Market** — Question or market ID
- **Spread** — Size (color: green ≥$0.03, yellow ≥$0.01)
- **Dur** — Duration (ms or seconds)
- **Class** — P1/P2/P3/P4 or noise (color-coded)
- **Reason** — Close reason (vol-grad, vol-inst, drift, vanished)
- **μs** — Detection latency in microseconds (green &lt;500, yellow &lt;2000, red otherwise)

---

## Header

| Field | Source | Meaning |
|-------|--------|---------|
| Status | API connectivity | ● connected / ◌ connecting / ✗ error |
| WS ✓/✗ | `/health` | Scanner WebSocket connected to Polymarket |
| Hydrated | `/health` | Markets with both YES/NO prices received |
| Windows today | `/stats/summary` | Total windows opened today |
| Avg | `/stats/summary` | Average window duration today |
| p99 | `/stats/latency` | 99th percentile detection latency (ms) |
| Queue | `/health` | Pending window writes to DB |
| Markets | `/stats/summary` | Total markets tracked |

---

## Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `q` / `Q` | Quit |
| `r` / `R` | Manual refresh |
| `Tab` / `Shift+Tab` | Switch focus between Markets and Windows panes |
| `↑` / `↓` or `k` / `j` | Scroll within focused pane |
| `Enter` | (Markets pane) Load windows for selected market |
| `Esc` | (Windows pane) Return to recent windows list |

---

## Data Fetching

On startup and every **2 seconds**, the TUI issues 6 parallel HTTP requests:

- `GET /stats/summary` — Totals and top markets
- `GET /windows/recent?limit=100` — Recent closed windows
- `GET /markets` — All markets with stats
- `GET /health` — WS status, hydrated count, write queue
- `GET /stats/latency` — p50/p95/p99
- `GET /windows/open` — Currently open windows

If the core requests (summary, windows, markets) fail, the connection status shows an error and data is not updated. Health and latency are optional — they populate when available.

---

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `API_URL` | `http://localhost:3000` | Scanner HTTP API base URL |

---

## Tech Stack

- **ratatui** — Terminal UI rendering
- **crossterm** — Raw mode, alternate screen, key events
- **reqwest** — HTTP client for API polling

The TUI runs in the terminal alternate screen and restores the previous screen on exit.
