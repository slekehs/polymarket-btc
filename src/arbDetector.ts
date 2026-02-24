import { Market } from './markets';
import { OrderbookWatcher } from './websocket';
import { config } from './config';
import { scanLog, verboseLog, tickLog, arbTickLog } from './log';

export interface ArbEvent {
  timestamp: string;
  marketId: string;
  question: string;
  timeframe: string;
  yesAsk: number;
  noAsk: number;
  combined: number;
  spread: number;
  isArb: boolean;
}

export interface ArbWindow {
  windowStart: string;
  windowEnd: string;
  /** Time from first to last arb tick (ms). 0 means a single-tick flash. */
  durationMs: number;
  marketId: string;
  question: string;
  timeframe: string;
  tickCount: number;
  peakSpread: number;
  avgSpread: number;
  openCombined: number;
  closeCombined: number;
}

interface WindowState {
  startTs: number;
  lastArbTickTs: number;
  openCombined: number;
  spreads: number[];
}

export interface MarketStats {
  marketId: string;
  question: string;
  timeframe: string;
  startAt: string;
  tickCount: number;
  windowCount: number;
  bestSpread: number;
  sumArbSpread: number;
  arbTickCount: number;
  avgWindowDurationMs: number;
  longestWindowMs: number;
  sumWindowDurationMs: number;
  lastArbAt: number | null;
  currentCombined: number | null;
}

export class ArbDetector {
  private markets = new Map<string, Market>();
  private tokenToMarket = new Map<string, string>();
  private stats = new Map<string, MarketStats>();
  private windows = new Map<string, WindowState>();
  private watcher: OrderbookWatcher;
  private onEvent: (event: ArbEvent) => void;
  private onWindow: (window: ArbWindow) => void;

  constructor(
    watcher: OrderbookWatcher,
    onEvent: (event: ArbEvent) => void,
    onWindow: (window: ArbWindow) => void,
  ) {
    this.watcher = watcher;
    this.onEvent = onEvent;
    this.onWindow = onWindow;
  }

  addMarkets(markets: Market[]) {
    for (const m of markets) {
      if (this.markets.has(m.marketId)) continue;
      scanLog(`[Market] Watching: [${m.timeframe}] ${m.question}`);
      this.markets.set(m.marketId, m);
      this.tokenToMarket.set(m.yesTokenId, m.marketId);
      this.tokenToMarket.set(m.noTokenId, m.marketId);
      this.stats.set(m.marketId, {
        marketId: m.marketId,
        question: m.question,
        timeframe: m.timeframe,
        startAt: m.startAt,
        tickCount: 0,
        windowCount: 0,
        bestSpread: 0,
        sumArbSpread: 0,
        arbTickCount: 0,
        avgWindowDurationMs: 0,
        longestWindowMs: 0,
        sumWindowDurationMs: 0,
        lastArbAt: null,
        currentCombined: null,
      });
    }
  }

  check(tokenId: string) {
    const marketId = this.tokenToMarket.get(tokenId);
    if (!marketId) return;

    const market = this.markets.get(marketId);
    if (!market) return;

    const yesAsk = this.watcher.getBestAsk(market.yesTokenId);
    const noAsk = this.watcher.getBestAsk(market.noTokenId);
    if (yesAsk === null || noAsk === null) return;

    const yesBid = this.watcher.getBestBid(market.yesTokenId);
    const noBid = this.watcher.getBestBid(market.noTokenId);

    const combined = yesAsk + noAsk;
    const spread = 1.0 - combined;
    const isArb = combined < config.MIN_COMBINED_THRESHOLD;
    const inWindow = this.windows.has(marketId);
    const now = Date.now();

    const s = this.stats.get(marketId)!;
    s.tickCount++;
    s.currentCombined = combined;

    if (isArb && !inWindow) {
      // Window opens
      this.windows.set(marketId, {
        startTs: now,
        lastArbTickTs: now,
        openCombined: combined,
        spreads: [spread],
      });
      s.lastArbAt = now;
      s.sumArbSpread += spread;
      s.arbTickCount++;
      if (spread > s.bestSpread) s.bestSpread = spread;
      scanLog(
        `[ARB OPEN] combined=${combined.toFixed(4)} spread=${spread.toFixed(4)} | ` +
          `YES=${yesAsk.toFixed(4)} NO=${noAsk.toFixed(4)} | ` +
          `[${market.timeframe}] ${market.question}`,
      );
    } else if (isArb && inWindow) {
      // Window continues — accumulate and advance lastArbTickTs
      const w = this.windows.get(marketId)!;
      w.spreads.push(spread);
      w.lastArbTickTs = now;
      s.lastArbAt = now;
      s.sumArbSpread += spread;
      s.arbTickCount++;
      if (spread > s.bestSpread) s.bestSpread = spread;
    } else if (!isArb && inWindow) {
      // Window closes — duration is honest: last arb tick minus first arb tick
      this.closeWindow(marketId, market, combined);
    }

    const fmtBid = (v: number | null) => (v !== null ? v.toFixed(4) : '—');
    const tickMsg =
      `[tick] UP bid=${fmtBid(yesBid)} ask=${yesAsk.toFixed(4)} | ` +
      `DOWN bid=${fmtBid(noBid)} ask=${noAsk.toFixed(4)} | ` +
      `combined_ask=${combined.toFixed(4)}${isArb ? ' *** ARB ***' : ''} | ` +
      `[${market.timeframe}] ${market.question}`;
    tickLog(isArb ? `\x1b[1;32m${tickMsg}\x1b[0m` : tickMsg);
    if (isArb) arbTickLog(tickMsg);

    verboseLog(
      `[tick] combined=${combined.toFixed(4)} YES=${yesAsk.toFixed(4)} NO=${noAsk.toFixed(4)}` +
        `${isArb ? ' *** ARB ***' : ''} | [${market.timeframe}] ${market.question}`,
    );

    this.onEvent({
      timestamp: new Date().toISOString(),
      marketId,
      question: market.question,
      timeframe: market.timeframe,
      yesAsk,
      noAsk,
      combined,
      spread,
      isArb,
    });
  }

  private closeWindow(marketId: string, market: Market, closeCombined: number) {
    const w = this.windows.get(marketId);
    if (!w) return;
    this.windows.delete(marketId);

    // Discard windows that don't meet the minimum tick threshold — these are
    // single-tick price glitches, not real tradeable windows.
    if (w.spreads.length < config.MIN_ARB_TICKS) return;

    // Honest duration: time from first arb tick to last arb tick.
    const durationMs = w.lastArbTickTs - w.startTs;
    const peakSpread = Math.max(...w.spreads);
    const avgSpread = w.spreads.reduce((a, b) => a + b, 0) / w.spreads.length;

    const s = this.stats.get(marketId)!;
    s.windowCount++;
    s.sumWindowDurationMs += durationMs;
    s.avgWindowDurationMs = s.sumWindowDurationMs / s.windowCount;
    if (durationMs > s.longestWindowMs) s.longestWindowMs = durationMs;
    if (peakSpread > s.bestSpread) s.bestSpread = peakSpread;

    const arbWindow: ArbWindow = {
      windowStart: new Date(w.startTs).toISOString(),
      windowEnd: new Date(w.lastArbTickTs).toISOString(),
      durationMs,
      marketId,
      question: market.question,
      timeframe: market.timeframe,
      tickCount: w.spreads.length,
      peakSpread,
      avgSpread,
      openCombined: w.openCombined,
      closeCombined,
    };

    scanLog(
      `[ARB CLOSE] duration=${durationMs}ms ticks=${w.spreads.length} ` +
        `peak=${peakSpread.toFixed(4)} avg=${avgSpread.toFixed(4)} | ` +
        `[${market.timeframe}] ${market.question}`,
    );

    this.onWindow(arbWindow);
  }

  removeMarkets(markets: Market[]) {
    for (const m of markets) {
      if (this.windows.has(m.marketId)) {
        const currentCombined = this.stats.get(m.marketId)?.currentCombined ?? 1.0;
        this.closeWindow(m.marketId, m, currentCombined);
      }
      this.markets.delete(m.marketId);
      this.stats.delete(m.marketId);
      this.tokenToMarket.delete(m.yesTokenId);
      this.tokenToMarket.delete(m.noTokenId);
    }
  }

  getAllStats(): MarketStats[] {
    return Array.from(this.stats.values());
  }

  getWatchedCount(): number {
    return this.markets.size;
  }
}
