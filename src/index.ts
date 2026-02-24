import { config } from './config';
import { fetchMarkets, Market } from './markets';
import { OrderbookWatcher } from './websocket';
import { ArbDetector } from './arbDetector';
import { Logger } from './logger';
import { scanLog } from './log';

/**
 * For each timeframe group, returns:
 * - all currently live markets (startAt <= now < expiresAt)
 * - the single nearest upcoming market (startAt > now)
 *
 * This ensures the orderbook for the next market is already warm when
 * the current one expires, eliminating any gap in coverage.
 */
function selectWatchedMarkets(allMarkets: Market[]): Market[] {
  const now = Date.now();
  const byTimeframe = new Map<string, { live: Market[]; upcoming: Market[] }>();

  for (const m of allMarkets) {
    const start = m.startAt ? new Date(m.startAt).getTime() : 0;
    const end = m.expiresAt ? new Date(m.expiresAt).getTime() : Infinity;
    if (end <= now) continue;

    if (!byTimeframe.has(m.timeframe)) {
      byTimeframe.set(m.timeframe, { live: [], upcoming: [] });
    }
    const group = byTimeframe.get(m.timeframe)!;

    if (start <= now) {
      group.live.push(m);
    } else {
      group.upcoming.push(m);
    }
  }

  const selected: Market[] = [];
  for (const group of byTimeframe.values()) {
    selected.push(...group.live);

    if (group.upcoming.length > 0) {
      group.upcoming.sort(
        (a, b) => new Date(a.startAt).getTime() - new Date(b.startAt).getTime(),
      );
      selected.push(group.upcoming[0]);
    }
  }

  return selected;
}

async function main() {
  scanLog('[Scanner] Starting Polymarket BTC Arb Scanner...');
  scanLog(`[Scanner] Watching timeframes: ${config.WATCHED_TIMEFRAMES.join(', ')}`);
  scanLog(`[Scanner] Threshold: combined < ${config.MIN_COMBINED_THRESHOLD}`);
  scanLog(`[Scanner] Stats interval: ${config.STATS_INTERVAL / 1000}s`);
  scanLog(`[Scanner] CSV log: ${config.LOG_FILE}  |  Scanner log: ${config.SCANNER_LOG}`);

  let allMarkets: Market[] = [];
  try {
    scanLog('[Scanner] Fetching active BTC markets...');
    allMarkets = await fetchMarkets();
    scanLog(`[Scanner] Fetched ${allMarkets.length} markets (${config.WATCHED_TIMEFRAMES.join('/')})`);
    if (allMarkets.length === 0) {
      scanLog('[Scanner] No markets found — will retry on next refresh interval');
    }
  } catch (err) {
    scanLog(`[Scanner] Failed to fetch markets: ${err}`);
    process.exit(1);
  }

  let detector: ArbDetector;
  let logger: Logger;

  const watcher = new OrderbookWatcher((tokenId) => {
    detector.check(tokenId);
  });

  detector = new ArbDetector(
    watcher,
    (_event) => { /* per-tick event — no longer logged to CSV */ },
    (window) => { logger.logWindow(window); },
  );

  logger = new Logger(
    () => detector.getAllStats(),
    () => ({ markets: detector.getWatchedCount(), updates: watcher.getTotalUpdates() }),
  );

  let watchedMarkets = selectWatchedMarkets(allMarkets);
  detector.addMarkets(watchedMarkets);
  watcher.addMarkets(watchedMarkets);

  const watchedIds = new Set(watchedMarkets.map((m) => m.marketId));

  setInterval(async () => {
    try {
      // Fetch fresh market list and merge into allMarkets
      const fresh = await fetchMarkets();
      const knownIds = new Set(allMarkets.map((m) => m.marketId));
      const newOnes = fresh.filter((m) => !knownIds.has(m.marketId));
      if (newOnes.length > 0) {
        scanLog(`[Scanner] Discovered ${newOnes.length} new market(s)`);
        allMarkets.push(...newOnes);
      }

      // Prune fully expired markets from allMarkets
      const now = Date.now();
      allMarkets = allMarkets.filter(
        (m) => !m.expiresAt || new Date(m.expiresAt).getTime() > now,
      );

      // Recompute the desired watched set
      const desired = selectWatchedMarkets(allMarkets);
      const desiredIds = new Set(desired.map((m) => m.marketId));

      // Add markets that should now be watched but aren't yet
      const toAdd = desired.filter((m) => !watchedIds.has(m.marketId));
      if (toAdd.length > 0) {
        scanLog(`[Scanner] Adding ${toAdd.length} market(s) to watch`);
        detector.addMarkets(toAdd);
        watcher.addMarkets(toAdd);
        toAdd.forEach((m) => watchedIds.add(m.marketId));
      }

      // Remove markets that are no longer in the desired set (expired or replaced)
      const toRemove = watchedMarkets.filter((m) => !desiredIds.has(m.marketId));
      if (toRemove.length > 0) {
        scanLog(`[Scanner] Removing ${toRemove.length} market(s) from watch`);
        detector.removeMarkets(toRemove);
        watcher.removeMarkets(toRemove);
        toRemove.forEach((m) => watchedIds.delete(m.marketId));
      }

      watchedMarkets = desired;
    } catch (err) {
      scanLog(`[Scanner] Market refresh failed: ${err}`);
    }
  }, config.MARKET_REFRESH_INTERVAL);

  process.on('SIGINT', () => {
    scanLog('[Scanner] Shutting down...');
    watcher.stop();
    logger.printLeaderboard();
    process.exit(0);
  });

  logger.startPeriodicPrint();

  await watcher.connect();
}

main().catch((err) => {
  console.error('[Scanner] Fatal error:', err);
  process.exit(1);
});
