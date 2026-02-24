# Polymarket Arbitrage Bot — Product Requirements Document

## Overview

A fully automated arbitrage trading bot that monitors Polymarket's 5-minute BTC Up/Down binary markets, detects pricing inefficiencies where the combined cost of YES and NO shares falls below $1.00, and executes simultaneous trades on both sides to lock in guaranteed profit regardless of market outcome.

---

## Background & Strategy

In any binary prediction market, at settlement one outcome pays $1.00 and the other pays $0.00. This means the combined fair value of YES + NO is always exactly $1.00. When temporary order book imbalances cause the combined ask price to drop below $1.00, a risk-free arbitrage opportunity exists.

**Core mechanic:**
- Buy YES at ask price X
- Buy NO at ask price Y
- If X + Y < $1.00, profit is locked in regardless of BTC direction
- Hold both positions to expiry, collect $1.00 payout
- Net profit = $1.00 − (X + Y) − fees

**Target markets:** Polymarket 5-minute BTC Up/Down markets, which reset every 5 minutes and produce the most frequent pricing inefficiencies due to rapid trader activity.

---

## Goals

- Detect and execute arbitrage opportunities on Polymarket BTC 5-minute markets in real time
- Start with $200 USDC capital, trading $50/side per trade
- Target $200–$300/day net profit after fees
- Compound profits to scale position size over time
- Begin with a paper trading mode to validate strategy before deploying real capital
- Run on a home server initially, with easy migration to a Hetzner VPS in Germany

---

## Non-Goals

- Not targeting HFT-level microsecond latency
- Not trading any markets other than BTC 5-minute Up/Down on Polymarket
- Not building a UI or dashboard (CLI output and log files are sufficient for v1)
- Not implementing directional/predictive trading of any kind

---

## Phases

### Phase 1 — Paper Trading (Validation)
Build and run the bot in simulation mode against live Polymarket data. No real orders placed. Goal is to validate that opportunities exist at the expected frequency and size before risking capital.

### Phase 2 — Live Trading (Small Size)
Deploy with $200 USDC, trading $50/side. Prove real-world fill rates and actual net profit match paper trading projections.

### Phase 3 — Compounding & Scaling
Reinvest profits to grow position size. Scale toward $100–$200/side as balance grows, targeting $200–$300/day sustainably.

---

## Technical Requirements

### Stack
- **Language:** TypeScript (Node.js v20+)
- **Runtime:** Home server (Linux), later migrated to Hetzner VPS in Germany
- **Wallet:** Polygon wallet (MetaMask or programmatic via ethers.js)
- **Capital:** USDC on Polygon network
- **APIs:** Polymarket CLOB REST API + WebSocket

### Architecture

```
WebSocket Feed (live orderbook)
        ↓
Arb Detector (checks YES ask + NO ask < $1.00)
        ↓
Position Sizer (calculates trade size based on balance)
        ↓
Order Executor (parallel submit YES + NO orders)
        ↓
Position Tracker (monitors open positions)
        ↓
Logger / Stats Engine (P&L tracking, projections)
```

### Key Components

#### 1. Market Scanner
- Fetches active 5-minute BTC markets from Polymarket CLOB REST API on startup
- Refreshes market list every 5 minutes to catch newly opened markets
- Filters by question keywords: "BTC", "Bitcoin", "Up", "Down"
- Extracts YES and NO token IDs for WebSocket subscription

#### 2. WebSocket Listener
- Connects to `wss://ws-subscriptions-clob.polymarket.com/ws/market`
- Subscribes to live orderbook updates for all active BTC markets
- Processes incoming price updates in real time
- Auto-reconnects on disconnect with exponential backoff

#### 3. Arb Detector
- On each orderbook update, reads best ask for YES token and best ask for NO token
- Calculates combined price: `combined = yesAsk + noAsk`
- Calculates gross spread: `spread = 1.0 - combined`
- Calculates net spread after fees: `netSpread = spread - (tradeSize * 0.02 * 2 / tradeSize)`
- Only flags as opportunity if `netSpread >= 0.02` (2 cent minimum net profit per dollar)

#### 4. Order Executor (Phase 2+)
- Pre-approves USDC spending on startup (one-time, max approval)
- Submits YES and NO limit orders simultaneously using `Promise.all()`
- Sets order expiry to match market expiry time
- Handles partial fills: cancels unfilled side if one leg fills and other doesn't within 5 seconds
- Never sends sensitive credentials through any unencrypted channel

#### 5. Position Tracker
- Tracks all open positions (both legs filled)
- Monitors time to expiry for each position
- At settlement, records payout and calculates actual realized profit
- Tracks leg risk exposure for any unmatched partial fills

#### 6. Position Sizer
- Starts at $50/side
- Scales position size as balance grows according to this schedule:

| Balance | Trade Size Per Side |
|---------|---------------------|
| $100–$300 | $50 |
| $300–$600 | $100 |
| $600–$1,200 | $200 |
| $1,200+ | $300 |

#### 7. Logger & Stats Engine
- Logs every opportunity detected (whether traded or not) with timestamp, market, spread, and theoretical profit
- Tracks paper vs. real P&L separately
- Outputs running stats to console every 60 seconds:
  - Runtime
  - Markets watched
  - Orderbook updates processed
  - Arb opportunities detected
  - Trades executed
  - Gross profit, fees, net profit
  - Projected daily profit (extrapolated from runtime)
- Saves all trades and stats to `trades.json` on disk
- Appends all console output to `bot.log`

---

## Fee Model

Polymarket charges approximately 2% taker fee per side. All profit calculations must account for this.

```
Gross profit  = spread × tradeSize
Fees          = tradeSize × 0.02 × 2   (both sides)
Net profit    = grossProfit − fees
```

A trade is only executed if `netProfit > 0` after fees.

---

## Paper Trading Mode

Paper trading runs identically to live trading except no orders are submitted to Polymarket. It uses the same WebSocket feed, same arb detection logic, same fee calculations, and same logging. The only difference is the execution step is skipped and positions are marked as `paper`.

Paper trading output gives a realistic projection of:
- How many opportunities exist per day
- What the average spread size is
- What daily net profit would be at a given trade size

**Validation criteria before going live:**
- Run paper trader for minimum 48 hours
- Projected daily net profit must exceed $50 at $50/side trade size
- At least 30 arb opportunities detected per day
- Average net profit per trade must exceed $0.40

---

## Speed Requirements

At the $200–$300/day target, the bot needs to be fast enough to beat human reaction time, not other bots. Target execution pipeline:

| Step | Target Latency |
|------|---------------|
| WebSocket message received | — |
| Arb detection calculation | < 1ms |
| Order construction | < 5ms |
| Parallel order submission | < 150ms |
| **Total end-to-end** | **< 200ms** |

This is achievable on a home server with a standard internet connection. No colocated VPS required for Phase 1–2.

---

## Infrastructure

### Phase 1–2: Home Server
- Node.js v20+ installed
- Stable internet connection
- Bot runs as a background process (`pm2` or `screen`)
- No VPN required for paper trading
- VPN required for live trading (US users are geo-blocked on Polymarket)
  - Recommended: Mullvad or ProtonVPN, connect to Netherlands or Germany server

### Phase 3: Hetzner VPS Migration (Optional)
- Hetzner CX22 in Falkenstein, Germany (~€4–5/month)
- No VPN needed — EU IP bypasses geo-block natively
- Lower latency to Polymarket infrastructure
- 24/7 uptime without relying on home server availability

---

## Wallet & Security Requirements

- Use a dedicated Polygon wallet separate from any personal wallets
- Never connect the trading wallet to any US-linked accounts or services
- Pre-approve USDC spending once at startup — do not approve per-trade
- Store private key in a local `.env` file, never hardcoded
- `.env` must be in `.gitignore` — never committed to any repository
- Keep position sizes reasonable: if an account gets flagged, losses should be manageable

---

## Configuration (`.env`)

```
PRIVATE_KEY=your_polygon_wallet_private_key
TRADE_SIZE=50
MIN_NET_SPREAD=0.02
PAPER_MODE=true
STATS_INTERVAL=60000
LOG_FILE=trades.json
```

---

## Project Structure

```
polymarket-bot/
├── src/
│   ├── index.ts              # Entry point
│   ├── config.ts             # Loads env vars and constants
│   ├── markets.ts            # Fetches and filters active markets
│   ├── websocket.ts          # WebSocket connection and listener
│   ├── arbDetector.ts        # Core arb detection logic
│   ├── executor.ts           # Order submission (live mode)
│   ├── paperExecutor.ts      # Simulated execution (paper mode)
│   ├── positionTracker.ts    # Tracks open/closed positions
│   ├── positionSizer.ts      # Calculates trade size from balance
│   └── logger.ts             # Stats, console output, file logging
├── trades.json               # Auto-generated trade log
├── bot.log                   # Auto-generated console log
├── .env                      # Private config (never commit)
├── .env.example              # Template for .env
├── .gitignore
├── package.json
└── tsconfig.json
```

---

## Success Metrics

| Metric | Phase 1 Target | Phase 2 Target | Phase 3 Target |
|--------|---------------|----------------|----------------|
| Arbs detected per day | 50+ | 50+ | 100+ |
| Avg net profit per trade | $0.40+ (paper) | $0.40+ (real) | $0.80+ |
| Daily net profit | $20+ projected | $50–$100 | $200–$300 |
| Uptime | 90%+ | 95%+ | 99%+ |
| Leg risk incidents | N/A | < 2/day | < 1/day |

---

## Risks & Mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Polymarket geo-blocks home IP | Medium | VPN on live trades; migrate to Hetzner EU VPS |
| Account flagged for bot activity | Medium | Keep sizes reasonable; use non-KYC wallet |
| Partial fill leg risk | Medium | Auto-cancel other leg within 5 seconds if one side fails |
| Spread closes before both orders fill | Medium | Only trade spreads with sufficient size in orderbook |
| Polymarket changes API or fee structure | Low | Monitor API changelog; modular executor makes swapping easy |
| Insufficient liquidity at scale | Medium | Monitor fill rates as position size increases; don't scale faster than fill rate allows |

---

## Dependencies

```json
{
  "dependencies": {
    "ws": "^8.x",
    "ethers": "^6.x",
    "dotenv": "^16.x"
  },
  "devDependencies": {
    "typescript": "^5.x",
    "ts-node": "^10.x",
    "@types/ws": "^8.x",
    "@types/node": "^20.x"
  }
}
```

---

## Getting Started (Quick Reference)

```bash
# 1. Clone / create project
mkdir polymarket-bot && cd polymarket-bot
npm init -y

# 2. Install dependencies
npm install ws ethers dotenv
npm install -D typescript ts-node @types/ws @types/node
npx tsc --init

# 3. Create .env
cp .env.example .env
# Edit .env — set PAPER_MODE=true to start

# 4. Run paper trader
npx ts-node src/index.ts

# 5. Monitor output
tail -f bot.log
```

---

*Document version: 1.0 | Created: February 2026*
