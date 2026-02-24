import * as fs from 'fs';
import { config } from './config';

function writeAsync(filePath: string, line: string): void {
  fs.appendFile(filePath, line, (err) => {
    if (err) process.stderr.write(`[log] write failed (${filePath}): ${err.message}\n`);
  });
}

export function scanLog(message: string): void {
  const line = `[${new Date().toISOString()}] ${message}\n`;
  process.stdout.write(line);
  writeAsync(config.SCANNER_LOG, line);
}

export function verboseLog(message: string): void {
  if (!config.VERBOSE) return;
  const line = `[${new Date().toISOString()}] ${message}\n`;
  writeAsync(config.SCANNER_LOG, line);
}

export function tickLog(message: string): void {
  const line = `[${new Date().toISOString()}] ${message}\n`;
  writeAsync(config.FULL_SCANNER_LOG, line);
}

export function rawWsLog(message: string): void {
  const line = `[${new Date().toISOString()}] ${message}\n`;
  writeAsync(config.WS_RAW_LOG, line);
}

export function arbTickLog(message: string): void {
  const line = `[${new Date().toISOString()}] \x1b[1;32m${message}\x1b[0m\n`;
  writeAsync(config.ARB_EVENTS_LOG, line);
}
