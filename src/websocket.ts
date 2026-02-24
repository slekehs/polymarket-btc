import WebSocket from 'ws';
import { Market } from './markets';
import { scanLog, rawWsLog } from './log';
import { config } from './config';

const WS_URL = 'wss://ws-subscriptions-clob.polymarket.com/ws/market';

class OrderBook {
  private asks = new Map<number, number>();
  private bids = new Map<number, number>();

  applySnapshot(
    asks: Array<{ price: string; size: string }>,
    bids: Array<{ price: string; size: string }>,
  ) {
    this.asks.clear();
    for (const a of asks) {
      const size = parseFloat(a.size);
      if (size > 0) this.asks.set(parseFloat(a.price), size);
    }
    this.bids.clear();
    for (const b of bids) {
      const size = parseFloat(b.size);
      if (size > 0) this.bids.set(parseFloat(b.price), size);
    }
  }

  applyChange(price: number, side: string, size: number) {
    if (side === 'SELL') {
      if (size === 0) this.asks.delete(price);
      else this.asks.set(price, size);
    } else if (side === 'BUY') {
      if (size === 0) this.bids.delete(price);
      else this.bids.set(price, size);
    }
  }

  getBestAsk(): number | null {
    if (this.asks.size === 0) return null;
    let best = Infinity;
    for (const price of this.asks.keys()) {
      if (price < best) best = price;
    }
    return best === Infinity ? null : best;
  }

  getBestBid(): number | null {
    if (this.bids.size === 0) return null;
    let best = -Infinity;
    for (const price of this.bids.keys()) {
      if (price > best) best = price;
    }
    return best === -Infinity ? null : best;
  }
}

export type UpdateCallback = (tokenId: string) => void;

export class OrderbookWatcher {
  private ws: WebSocket | null = null;
  private books = new Map<string, OrderBook>();
  private bestAsks = new Map<string, number>();
  private bestBids = new Map<string, number>();
  private subscribedTokens = new Set<string>();
  private onUpdate: UpdateCallback;
  private reconnectDelay = 1_000;
  private shouldRun = true;
  private totalUpdates = 0;

  constructor(onUpdate: UpdateCallback) {
    this.onUpdate = onUpdate;
  }

  getBestAsk(tokenId: string): number | null {
    return this.bestAsks.get(tokenId) ?? null;
  }

  getBestBid(tokenId: string): number | null {
    return this.bestBids.get(tokenId) ?? null;
  }

  getTotalUpdates(): number {
    return this.totalUpdates;
  }

  addMarkets(markets: Market[]) {
    const newTokens: string[] = [];
    for (const m of markets) {
      for (const tokenId of [m.yesTokenId, m.noTokenId]) {
        if (!this.subscribedTokens.has(tokenId)) {
          this.subscribedTokens.add(tokenId);
          this.books.set(tokenId, new OrderBook());
          newTokens.push(tokenId);
        }
      }
    }
    if (newTokens.length > 0 && this.ws?.readyState === WebSocket.OPEN) {
      this.sendSubscription(newTokens);
    }
  }

  removeMarkets(markets: Market[]) {
    for (const m of markets) {
      for (const tokenId of [m.yesTokenId, m.noTokenId]) {
        this.subscribedTokens.delete(tokenId);
        this.books.delete(tokenId);
        this.bestAsks.delete(tokenId);
        this.bestBids.delete(tokenId);
      }
    }
  }

  private sendSubscription(tokenIds: string[]) {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;
    this.ws.send(JSON.stringify({ assets_ids: tokenIds, type: 'Book' }));
  }

  async connect(): Promise<void> {
    while (this.shouldRun) {
      try {
        await this.connectOnce();
      } catch (err) {
        scanLog(`[WS] Error: ${err}`);
      }
      if (!this.shouldRun) break;
      scanLog(`[WS] Reconnecting in ${this.reconnectDelay / 1000}s...`);
      await sleep(this.reconnectDelay);
      this.reconnectDelay = Math.min(this.reconnectDelay * 2, 30_000);
    }
  }

  private connectOnce(): Promise<void> {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(WS_URL);
      this.ws = ws;

      let pingInterval: NodeJS.Timeout;

      ws.on('open', () => {
        scanLog(`[WS] Connected — subscribing to ${this.subscribedTokens.size} tokens`);
        this.reconnectDelay = 1_000;
        const allTokens = Array.from(this.subscribedTokens);
        if (allTokens.length > 0) this.sendSubscription(allTokens);

        pingInterval = setInterval(() => {
          if (ws.readyState === WebSocket.OPEN) ws.ping();
        }, 30_000);
      });

      ws.on('message', (raw: WebSocket.RawData) => {
        const rawStr = raw.toString();
        if (config.WS_RAW_LOG) rawWsLog(rawStr);
        try {
          const payload = JSON.parse(rawStr);
          const msgs: unknown[] = Array.isArray(payload) ? payload : [payload];
          for (const msg of msgs) this.handleMessage(msg as Record<string, unknown>);
        } catch {
          // ignore malformed messages
        }
      });

      ws.on('close', () => {
        clearInterval(pingInterval);
        resolve();
      });

      ws.on('error', (err) => {
        clearInterval(pingInterval);
        reject(err);
      });
    });
  }

  private handleMessage(msg: Record<string, unknown>) {
    const tokenId = msg.asset_id as string | undefined;
    if (!tokenId) return;

    const book = this.books.get(tokenId);
    if (!book) return;

    if (msg.event_type === 'book') {
      const asks = (msg.asks ?? []) as Array<{ price: string; size: string }>;
      const bids = (msg.bids ?? []) as Array<{ price: string; size: string }>;
      book.applySnapshot(asks, bids);
      this.updateBestPrices(tokenId, book);
    } else if (msg.event_type === 'price_change') {
      const changes = (msg.changes ?? []) as Array<{ price: string; side: string; size: string }>;
      for (const c of changes) {
        book.applyChange(parseFloat(c.price), c.side, parseFloat(c.size));
      }
      this.updateBestPrices(tokenId, book);
    }
  }

  private updateBestPrices(tokenId: string, book: OrderBook) {
    const bestAsk = book.getBestAsk();
    const bestBid = book.getBestBid();
    const hadAsk = this.bestAsks.has(tokenId);

    if (bestAsk !== null) this.bestAsks.set(tokenId, bestAsk);
    if (bestBid !== null) this.bestBids.set(tokenId, bestBid);

    if (bestAsk !== null || bestBid !== null) {
      this.totalUpdates++;
      if (!hadAsk && bestAsk !== null) {
        scanLog(
          `[WS] First price received for token ${tokenId.slice(0, 10)}... ask=${bestAsk.toFixed(4)}`,
        );
      }
      this.onUpdate(tokenId);
    }
  }

  stop() {
    this.shouldRun = false;
    this.ws?.close();
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms));
}
