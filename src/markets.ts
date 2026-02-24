import { config } from './config';

const GAMMA_API = 'https://gamma-api.polymarket.com';

export interface Market {
  marketId: string;
  question: string;
  timeframe: string;
  startAt: string;
  expiresAt: string;
  yesTokenId: string;  // "Up" token
  noTokenId: string;   // "Down" token
}

function parseTimeframe(title: string): string {
  // Parse time range from "Bitcoin Up or Down - February 23, 7:35PM-7:40PM ET"
  const match = title.match(/(\d+):(\d+)(AM|PM)-(\d+):(\d+)(AM|PM)/i);
  if (!match) return 'unknown';

  let startH = parseInt(match[1]);
  const startM = parseInt(match[2]);
  const startPeriod = match[3].toUpperCase();
  let endH = parseInt(match[4]);
  const endM = parseInt(match[5]);
  const endPeriod = match[6].toUpperCase();

  if (startPeriod === 'PM' && startH !== 12) startH += 12;
  if (startPeriod === 'AM' && startH === 12) startH = 0;
  if (endPeriod === 'PM' && endH !== 12) endH += 12;
  if (endPeriod === 'AM' && endH === 12) endH = 0;

  const diffMin = endH * 60 + endM - (startH * 60 + startM);
  return diffMin > 0 ? `${diffMin}-min` : 'unknown';
}

function isBitcoinUpDown(title: string): boolean {
  const t = title.toLowerCase();
  return t.includes('bitcoin') && t.includes('up or down');
}

export async function fetchMarkets(): Promise<Market[]> {
  const markets: Market[] = [];
  const limit = 100;
  let offset = 0;
  let pageCount = 0;

  while (true) {
    const endDateMin = new Date().toISOString();
    const url =
      `${GAMMA_API}/events` +
      `?active=true&closed=false&limit=${limit}&offset=${offset}` +
      `&order=endDate&ascending=true&end_date_min=${endDateMin}&tag_slug=crypto`;

    const res = await fetch(url);
    if (!res.ok) throw new Error(`Gamma API ${res.status}: ${res.statusText}`);

    const body: unknown = await res.json();
    const events: any[] = Array.isArray(body) ? body : [];
    if (events.length === 0) break;

    for (const event of events) {
      const title: string = event.title ?? event.question ?? '';
      if (!isBitcoinUpDown(title)) continue;
      if (!event.active || event.closed) continue;

      const endDate: string = event.endDate ?? '';
      if (endDate && new Date(endDate).getTime() < Date.now()) continue;

      // Markets are nested under the event
      const subMarkets: any[] = event.markets ?? [];
      for (const m of subMarkets) {
        if (!m.active || m.closed) continue;
        if (!m.acceptingOrders) continue;

        let tokenIds: string[] = [];
        try {
          tokenIds = JSON.parse(m.clobTokenIds ?? '[]');
        } catch {
          continue;
        }
        if (tokenIds.length < 2) continue;

        // outcomes[0] = "Up", outcomes[1] = "Down"
        let outcomes: string[] = [];
        try {
          outcomes = JSON.parse(m.outcomes ?? '[]');
        } catch {
          outcomes = ['Up', 'Down'];
        }

        const upIdx = outcomes.findIndex((o: string) => o.toLowerCase() === 'up');
        const downIdx = outcomes.findIndex((o: string) => o.toLowerCase() === 'down');
        const upTokenId = tokenIds[upIdx >= 0 ? upIdx : 0];
        const downTokenId = tokenIds[downIdx >= 0 ? downIdx : 1];

        const timeframe = parseTimeframe(title);
        if (!config.WATCHED_TIMEFRAMES.includes(timeframe)) continue;

        // m.endDate is the authoritative window close time.
        // startAt is derived by subtracting the timeframe duration so that
        // selectWatchedMarkets() can correctly classify live vs upcoming markets.
        const subEndDate: string = m.endDate ?? endDate;
        const timeframeMins = parseInt(timeframe, 10);
        const startAt =
          subEndDate && timeframeMins > 0
            ? new Date(new Date(subEndDate).getTime() - timeframeMins * 60 * 1000).toISOString()
            : '';

        markets.push({
          marketId: m.conditionId ?? event.conditionId,
          question: title,
          timeframe,
          startAt,
          expiresAt: subEndDate,
          yesTokenId: upTokenId,
          noTokenId: downTokenId,
        });
      }
    }

    if (events.length < limit) break;
    offset += limit;
    pageCount++;
    if (pageCount >= 10) break;
  }

  return markets;
}
