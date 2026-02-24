import * as dotenv from 'dotenv';

dotenv.config();

const rawTimeframes = process.env.WATCHED_TIMEFRAMES ?? '5-min,15-min';

export const config = {
  MIN_COMBINED_THRESHOLD: parseFloat(process.env.MIN_COMBINED_THRESHOLD ?? '1.00'),
  STATS_INTERVAL: parseInt(process.env.STATS_INTERVAL ?? '30000', 10),
  LOG_FILE: process.env.LOG_FILE ?? 'arb_log.csv',
  SCANNER_LOG: process.env.SCANNER_LOG ?? 'scanner.log',
  FULL_SCANNER_LOG: process.env.FULL_SCANNER_LOG ?? 'full_scanner.log',
  WS_RAW_LOG: process.env.WS_RAW_LOG ?? 'ws_raw.log',
  VERBOSE: process.env.VERBOSE === 'true',
  MARKET_REFRESH_INTERVAL: parseInt(process.env.MARKET_REFRESH_INTERVAL ?? '30000', 10),
  WATCHED_TIMEFRAMES: rawTimeframes.split(',').map((s) => s.trim()).filter(Boolean),
  ARB_WINDOWS_LOG: process.env.ARB_WINDOWS_LOG ?? 'arb_windows.csv',
  ARB_EVENTS_LOG: process.env.ARB_EVENTS_LOG ?? 'arb_events.log',
  MIN_ARB_TICKS: parseInt(process.env.MIN_ARB_TICKS ?? '2', 10),
};
