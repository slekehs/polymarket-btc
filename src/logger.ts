import * as fs from 'fs';
import * as path from 'path';
import { ArbWindow, MarketStats } from './arbDetector';
import { config } from './config';
import { scanLog } from './log';

const WINDOWS_CSV_HEADER =
  'windowStart,windowEnd,durationMs,marketId,question,timeframe,' +
  'tickCount,peakSpread,avgSpread,openCombined,closeCombined\n';

interface TimeframeStats {
  windowCount: number;
  totalDurationMs: number;
  longestWindowMs: number;
  sumPeakSpread: number;
  sumAvgSpread: number;
  marketsObserved: Set<string>;
}

function emptyTimeframeStats(): TimeframeStats {
  return {
    windowCount: 0,
    totalDurationMs: 0,
    longestWindowMs: 0,
    sumPeakSpread: 0,
    sumAvgSpread: 0,
    marketsObserved: new Set(),
  };
}

function accumulateWindow(map: Map<string, TimeframeStats>, w: ArbWindow) {
  if (!map.has(w.timeframe)) map.set(w.timeframe, emptyTimeframeStats());
  const tf = map.get(w.timeframe)!;
  tf.windowCount++;
  tf.totalDurationMs += w.durationMs;
  if (w.durationMs > tf.longestWindowMs) tf.longestWindowMs = w.durationMs;
  tf.sumPeakSpread += w.peakSpread;
  tf.sumAvgSpread += w.avgSpread;
  tf.marketsObserved.add(w.marketId);
}

/** Minimal CSV line parser that handles double-quoted fields. */
function parseCsvLine(line: string): string[] {
  const fields: string[] = [];
  let current = '';
  let inQuotes = false;
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (ch === '"') {
      if (inQuotes && line[i + 1] === '"') { current += '"'; i++; }
      else inQuotes = !inQuotes;
    } else if (ch === ',' && !inQuotes) {
      fields.push(current); current = '';
    } else {
      current += ch;
    }
  }
  fields.push(current);
  return fields;
}

export class Logger {
  private windowsFile: string;
  private startTime = Date.now();
  private totalWindows = 0;
  private sessionByTimeframe = new Map<string, TimeframeStats>();
  private lifetimeByTimeframe = new Map<string, TimeframeStats>();
  private lifetimeWindowsAtStart = 0;
  private getStats: () => MarketStats[];
  private getWatcherInfo: () => { markets: number; updates: number };

  constructor(
    getStats: () => MarketStats[],
    getWatcherInfo: () => { markets: number; updates: number },
  ) {
    this.windowsFile = config.ARB_WINDOWS_LOG;
    this.getStats = getStats;
    this.getWatcherInfo = getWatcherInfo;
    this.initCsv();
    this.loadLifetimeTotals();
  }

  private initCsv() {
    if (!fs.existsSync(this.windowsFile)) {
      fs.writeFileSync(this.windowsFile, WINDOWS_CSV_HEADER);
    }
  }

  private loadLifetimeTotals() {
    if (!fs.existsSync(this.windowsFile)) return;
    const lines = fs.readFileSync(this.windowsFile, 'utf8').split('\n');
    let count = 0;
    for (let i = 1; i < lines.length; i++) {
      const line = lines[i].trim();
      if (!line) continue;
      const fields = parseCsvLine(line);
      if (fields.length < 11) continue;
      // CSV columns: windowStart(0) windowEnd(1) durationMs(2) marketId(3)
      //              question(4) timeframe(5) tickCount(6) peakSpread(7)
      //              avgSpread(8) openCombined(9) closeCombined(10)
      const durationMs = parseInt(fields[2], 10);
      const marketId = fields[3];
      const timeframe = fields[5];
      const peakSpread = parseFloat(fields[7]);
      const avgSpread = parseFloat(fields[8]);
      if (!timeframe || isNaN(durationMs) || isNaN(peakSpread)) continue;

      accumulateWindow(this.lifetimeByTimeframe, {
        windowStart: fields[0],
        windowEnd: fields[1],
        durationMs,
        marketId,
        question: fields[4],
        timeframe,
        tickCount: parseInt(fields[6], 10),
        peakSpread,
        avgSpread,
        openCombined: parseFloat(fields[9]),
        closeCombined: parseFloat(fields[10]),
      });
      count++;
    }
    this.lifetimeWindowsAtStart = count;
    if (count > 0) {
      scanLog(`[Logger] Loaded ${count} historical windows from CSV`);
    }
  }

  logWindow(window: ArbWindow) {
    this.totalWindows++;
    accumulateWindow(this.sessionByTimeframe, window);
    accumulateWindow(this.lifetimeByTimeframe, window);

    const row =
      [
        window.windowStart,
        window.windowEnd,
        window.durationMs,
        window.marketId,
        `"${window.question.replace(/"/g, '""')}"`,
        window.timeframe,
        window.tickCount,
        window.peakSpread.toFixed(4),
        window.avgSpread.toFixed(4),
        window.openCombined.toFixed(4),
        window.closeCombined.toFixed(4),
      ].join(',') + '\n';

    fs.appendFile(this.windowsFile, row, (err) => {
      if (err) process.stderr.write(`[logger] CSV write failed: ${err.message}\n`);
    });
  }

  printLeaderboard() {
    const allStats = this.getStats();
    const info = this.getWatcherInfo();
    const runtimeStr = formatDuration(Date.now() - this.startTime);
    const now = Date.now();

    const liveStats = allStats.filter(
      (s) => !s.startAt || new Date(s.startAt).getTime() <= now,
    );
    const sorted = [...liveStats].sort(
      (a, b) => b.windowCount - a.windowCount || b.longestWindowMs - a.longestWindowMs,
    );

    console.clear();
    console.log(`=== Polymarket BTC Arb Scanner  runtime: ${runtimeStr} ===`);
    console.log(
      `Markets: ${info.markets}  |  Ticks: ${info.updates.toLocaleString()}  |  Arb windows: ${this.totalWindows} this session  |  Lifetime: ${this.lifetimeWindowsAtStart + this.totalWindows}  |  Threshold: combined < ${config.MIN_COMBINED_THRESHOLD}`,
    );
    console.log('');

    // ── CURRENT MARKETS ──────────────────────────────────────────────────────
    console.log('CURRENT MARKETS:');
    const col = {
      rank: 5, tf: 9, wins: 9, avgDur: 10, longest: 10,
      bestSpread: 12, avgSpread: 11, last: 12, q: 42,
    };
    const mktHeader =
      'RANK'.padEnd(col.rank) + 'TF'.padEnd(col.tf) +
      'WINDOWS'.padStart(col.wins) + 'AVG DUR'.padStart(col.avgDur) +
      'LONGEST'.padStart(col.longest) + 'BEST SPREAD'.padStart(col.bestSpread) +
      'AVG SPREAD'.padStart(col.avgSpread) + 'LAST ARB'.padStart(col.last) + '  QUESTION';
    console.log(mktHeader);
    console.log('─'.repeat(123));
    if (sorted.length === 0) {
      console.log('  Waiting for market data...');
    } else {
      for (let i = 0; i < Math.min(sorted.length, 10); i++) {
        const s = sorted[i];
        const q = s.question.length > col.q ? s.question.slice(0, col.q - 3) + '...' : s.question;
        console.log(
          String(i + 1).padEnd(col.rank) +
          s.timeframe.padEnd(col.tf) +
          String(s.windowCount).padStart(col.wins) +
          (s.windowCount > 0 ? formatMs(s.avgWindowDurationMs) : '—').padStart(col.avgDur) +
          (s.longestWindowMs > 0 ? formatMs(s.longestWindowMs) : '—').padStart(col.longest) +
          (s.bestSpread > 0 ? s.bestSpread.toFixed(4) : '—').padStart(col.bestSpread) +
          (s.arbTickCount > 0 ? (s.sumArbSpread / s.arbTickCount).toFixed(4) : '—').padStart(col.avgSpread) +
          (s.lastArbAt ? formatAgo(s.lastArbAt) : '—').padStart(col.last) +
          '  ' + q,
        );
      }
    }

    // ── SHARED TABLE RENDERER ─────────────────────────────────────────────────
    const renderTfTable = (map: Map<string, TimeframeStats>) => {
      const scol = { tf: 9, wins: 9, avgDur: 10, longest: 10, bestSpread: 12, avgSpread: 11, markets: 14 };
      console.log(
        'TF'.padEnd(scol.tf) + 'WINDOWS'.padStart(scol.wins) +
        'AVG DUR'.padStart(scol.avgDur) + 'LONGEST'.padStart(scol.longest) +
        'BEST SPREAD'.padStart(scol.bestSpread) + 'AVG SPREAD'.padStart(scol.avgSpread) +
        'MARKETS SEEN'.padStart(scol.markets),
      );
      console.log('─'.repeat(78));
      if (map.size === 0) {
        console.log('  No arb windows recorded yet.');
        return;
      }
      const sorted = [...map.entries()].sort((a, b) => b[1].windowCount - a[1].windowCount);
      for (const [tf, data] of sorted) {
        console.log(
          tf.padEnd(scol.tf) +
          String(data.windowCount).padStart(scol.wins) +
          (data.windowCount > 0 ? formatMs(data.totalDurationMs / data.windowCount) : '—').padStart(scol.avgDur) +
          (data.longestWindowMs > 0 ? formatMs(data.longestWindowMs) : '—').padStart(scol.longest) +
          (data.windowCount > 0 ? (data.sumPeakSpread / data.windowCount).toFixed(4) : '—').padStart(scol.bestSpread) +
          (data.windowCount > 0 ? (data.sumAvgSpread / data.windowCount).toFixed(4) : '—').padStart(scol.avgSpread) +
          String(data.marketsObserved.size).padStart(scol.markets),
        );
      }
    };

    console.log('');
    console.log('SESSION TOTALS BY TIMEFRAME:');
    renderTfTable(this.sessionByTimeframe);

    console.log('');
    console.log(`LIFETIME TOTALS BY TIMEFRAME  (${this.lifetimeWindowsAtStart + this.totalWindows} windows all time):`);
    renderTfTable(this.lifetimeByTimeframe);

    console.log('');
    console.log(`Windows CSV: ${path.resolve(this.windowsFile)}`);

    scanLog(
      `[Stats] runtime: ${runtimeStr} | markets: ${info.markets} | ticks: ${info.updates.toLocaleString()} | arb windows: ${this.totalWindows}`,
    );
  }

  startPeriodicPrint() {
    this.printLeaderboard();
    setInterval(() => this.printLeaderboard(), config.STATS_INTERVAL);
  }
}

function formatDuration(ms: number): string {
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h > 0) return `${h}h ${m}m ${sec}s`;
  if (m > 0) return `${m}m ${sec}s`;
  return `${sec}s`;
}

function formatMs(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const m = Math.floor(ms / 60_000);
  const s = Math.floor((ms % 60_000) / 1000);
  return `${m}m ${s}s`;
}

function formatAgo(ts: number): string {
  const s = Math.floor((Date.now() - ts) / 1000);
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  return `${h}h ago`;
}
