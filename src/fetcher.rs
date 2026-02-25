use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

use crate::config::{CLOB_API_URL, Config};
use crate::error::{AppError, Result};
use crate::state::market_store::MarketStore;
use crate::types::{Category, Market};

/// Fetch active markets from the GAMMA REST API, applying quality filters.
/// Orders by volume_24hr descending so we fill `scanner_max_markets` with the
/// highest-activity markets first, then stops once the cap is reached.
pub async fn fetch_markets(cfg: &Config) -> Result<Vec<Market>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let min_expiry_secs = cfg.scanner_min_expiry_minutes * 60.0;
    let max_expiry_secs = cfg.scanner_max_expiry_hours * 3600.0;

    let mut markets = Vec::new();
    let mut offset = 0usize;
    let page_size = 500usize;

    'outer: loop {
        let url = format!(
            "{}/markets?active=true&closed=false&limit={}&offset={}&order=volume24hr&ascending=false",
            cfg.gamma_api_url, page_size, offset
        );

        let resp: serde_json::Value = client.get(&url).send().await?.json().await?;

        let items = match resp.as_array() {
            Some(a) => a.clone(),
            None => {
                return Err(AppError::Bootstrap(
                    "GAMMA /markets response was not an array".to_string(),
                ))
            }
        };

        if items.is_empty() {
            break;
        }

        for item in &items {
            if let Some(market) = parse_gamma_market(item, cfg, now, min_expiry_secs, max_expiry_secs) {
                markets.push(market);
                if markets.len() >= cfg.scanner_max_markets {
                    break 'outer;
                }
            }
        }

        if items.len() < page_size {
            break;
        }
        offset += page_size;
    }

    Ok(markets)
}

/// Fetch all active markets whose slug starts with any of the given prefixes.
/// Returns `(Market, matched_prefix, end_ts)` where `end_ts` is the Unix timestamp
/// parsed from the last numeric segment of the slug (e.g. `btc-updown-5m-1772068500` → 1772068500).
///
/// The Gamma API does not support a slug filter parameter — params like `slug_contains`
/// are silently ignored. Instead we query the 300 most recently *created* markets
/// (order=startDate desc), which reliably surfaces rolling short-timeframe markets since
/// Polymarket creates them every few minutes with a fresh startDate.
pub async fn fetch_pinned_markets(
    cfg: &Config,
    slug_prefixes: &[String],
) -> Result<Vec<(Market, String, u64)>> {
    if slug_prefixes.is_empty() {
        return Ok(Vec::new());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let url = format!(
        "{}/markets?active=true&closed=false&limit=300&order=startDate&ascending=false",
        cfg.gamma_api_url
    );

    let resp: serde_json::Value = client.get(&url).send().await?.json().await?;

    let items = match resp.as_array() {
        Some(a) => a.clone(),
        None => return Ok(Vec::new()),
    };

    let mut results: Vec<(Market, String, u64)> = Vec::new();

    for item in &items {
        let slug = item.get("slug").and_then(|s| s.as_str()).unwrap_or("");
        let Some(prefix) = slug_prefixes.iter().find(|p| slug.starts_with(p.as_str())) else {
            continue;
        };
        let end_ts = parse_slug_end_ts(slug);
        if let Some(market) = parse_gamma_market_unfiltered(item) {
            results.push((market, prefix.clone(), end_ts));
        }
    }

    // Deduplicate by market id
    results.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    results.dedup_by(|a, b| a.0.id == b.0.id);

    Ok(results)
}

/// Extract the Unix timestamp from the last numeric segment of a slug.
/// `btc-updown-5m-1772068500` → 1772068500. Returns 0 if not present.
pub fn parse_slug_end_ts(slug: &str) -> u64 {
    slug.rsplit('-')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Parse the window duration in seconds from a slug prefix.
/// `btc-updown-5m` → 300, `eth-updown-15m` → 900, `sol-updown-1h` → 3600.
pub fn parse_prefix_duration_secs(prefix: &str) -> u64 {
    let segment = match prefix.rsplit('-').next() {
        Some(s) => s,
        None => return 300,
    };
    if let Some(n) = segment.strip_suffix('m') {
        return n.parse::<u64>().unwrap_or(5) * 60;
    }
    if let Some(n) = segment.strip_suffix('h') {
        return n.parse::<u64>().unwrap_or(1) * 3600;
    }
    300
}

/// Parse a Gamma market JSON object with no quality filters applied.
/// Returns None only if the market is structurally unusable (missing token IDs, etc.).
pub fn parse_gamma_market_unfiltered(v: &serde_json::Value) -> Option<Market> {
    let id = v.get("conditionId")?.as_str()?.to_string();

    let token_ids: Vec<String> =
        serde_json::from_str(v.get("clobTokenIds")?.as_str()?).ok()?;
    let outcomes: Vec<String> =
        serde_json::from_str(v.get("outcomes")?.as_str()?).ok()?;

    if token_ids.len() < 2 || outcomes.len() < 2 {
        return None;
    }

    // Rolling crypto markets use "Up"/"Down" instead of "Yes"/"No".
    // "Up" maps to the yes (long) side, "Down" to the no (short) side.
    let yes_idx = outcomes.iter().position(|o| {
        o.eq_ignore_ascii_case("Yes") || o.eq_ignore_ascii_case("Up")
    })?;
    let no_idx = outcomes.iter().position(|o| {
        o.eq_ignore_ascii_case("No") || o.eq_ignore_ascii_case("Down")
    })?;
    let yes_token_id = token_ids.get(yes_idx)?.clone();
    let no_token_id = token_ids.get(no_idx)?.clone();

    let question = v
        .get("question")
        .and_then(|q| q.as_str())
        .unwrap_or("")
        .to_string();

    let end_date_iso = v
        .get("endDateIso")
        .and_then(|e| e.as_str())
        .map(|s| s.to_string());

    let category = v
        .get("events")
        .and_then(|e| e.as_array())
        .and_then(|a| a.first())
        .and_then(|e| e.get("category"))
        .and_then(|c| c.as_str())
        .map(parse_category_str)
        .unwrap_or(Category::Crypto);

    let total_volume = v
        .get("volume")
        .and_then(|vl| vl.as_f64().or_else(|| vl.as_str().and_then(|s| s.parse().ok())));

    Some(Market {
        id,
        question,
        category,
        end_date_iso,
        total_volume,
        yes_token_id,
        no_token_id,
    })
}

pub fn parse_gamma_market(
    v: &serde_json::Value,
    cfg: &Config,
    now_secs: f64,
    min_expiry_secs: f64,
    max_expiry_secs: f64,
) -> Option<Market> {
    let id = v.get("conditionId")?.as_str()?.to_string();

    let token_ids: Vec<String> = serde_json::from_str(
        v.get("clobTokenIds")?.as_str()?
    ).ok()?;
    let outcomes: Vec<String> = serde_json::from_str(
        v.get("outcomes")?.as_str()?
    ).ok()?;

    if token_ids.len() < 2 || outcomes.len() < 2 {
        return None;
    }

    let yes_idx = outcomes.iter().position(|o| {
        o.eq_ignore_ascii_case("Yes") || o.eq_ignore_ascii_case("Up")
    })?;
    let no_idx = outcomes.iter().position(|o| {
        o.eq_ignore_ascii_case("No") || o.eq_ignore_ascii_case("Down")
    })?;
    let yes_token_id = token_ids.get(yes_idx)?.clone();
    let no_token_id = token_ids.get(no_idx)?.clone();

    let volume_24h = v
        .get("volume24hr")
        .and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0.0);
    if volume_24h < cfg.scanner_min_volume_24h {
        return None;
    }

    let liquidity = v
        .get("liquidityNum")
        .and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0.0);
    if liquidity < cfg.scanner_min_liquidity {
        return None;
    }

    let end_date_iso = v
        .get("endDateIso")
        .and_then(|e| e.as_str())
        .map(|s| s.to_string());

    if let Some(ref end_str) = end_date_iso {
        match parse_iso_to_unix_secs(end_str) {
            Some(end_secs) => {
                let secs_until = end_secs - now_secs;
                if secs_until < min_expiry_secs || secs_until > max_expiry_secs {
                    return None;
                }
            }
            None => return None,
        }
    } else {
        return None;
    }

    let question = v
        .get("question")
        .and_then(|q| q.as_str())
        .unwrap_or("")
        .to_string();

    let category = v
        .get("events")
        .and_then(|e| e.as_array())
        .and_then(|a| a.first())
        .and_then(|e| e.get("category"))
        .and_then(|c| c.as_str())
        .map(parse_category_str)
        .unwrap_or(Category::Other);

    let total_volume = v
        .get("volume")
        .and_then(|vl| {
            vl.as_f64().or_else(|| vl.as_str().and_then(|s| s.parse().ok()))
        });

    Some(Market {
        id,
        question,
        category,
        end_date_iso,
        total_volume,
        yes_token_id,
        no_token_id,
    })
}

/// Fetch the CLOB REST order book for a sample of tokens and compare against
/// the WS-derived local book prices. Logs discrepancies to help verify data integrity.
pub async fn audit_book_prices(store: &Arc<MarketStore>, sample_count: usize) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("[BOOK AUDIT] failed to build HTTP client: {e}");
            return;
        }
    };

    let market_ids = store.all_market_ids();
    let sample: Vec<_> = market_ids.into_iter().take(sample_count).collect();

    for market_id in &sample {
        let Some(market) = store.get_market(market_id) else { continue };
        let Some((_, ws_yes_ask, ws_no_ask, ws_yes_bid, ws_no_bid)) =
            store.get_spread_inputs(&market.yes_token_id)
        else {
            continue;
        };

        let (rest_yes_ask, rest_yes_bid) = match fetch_rest_best_prices(&client, &market.yes_token_id).await {
            Some(p) => p,
            None => continue,
        };
        let (rest_no_ask, rest_no_bid) = match fetch_rest_best_prices(&client, &market.no_token_id).await {
            Some(p) => p,
            None => continue,
        };

        let ws_combined = ws_yes_ask + ws_no_ask;
        let rest_combined = rest_yes_ask + rest_no_ask;
        let ask_diff_yes = (ws_yes_ask - rest_yes_ask).abs();
        let ask_diff_no = (ws_no_ask - rest_no_ask).abs();

        let id_short = if market_id.len() > 12 { &market_id[..12] } else { market_id };
        info!(
            "[BOOK AUDIT] {id_short} | \
             WS: yes_ask={ws_yes_ask:.4} no_ask={ws_no_ask:.4} combined={ws_combined:.4} | \
             REST: yes_ask={rest_yes_ask:.4} no_ask={rest_no_ask:.4} combined={rest_combined:.4} | \
             diff: yes={ask_diff_yes:.4} no={ask_diff_no:.4}"
        );

        if ask_diff_yes > 0.005 || ask_diff_no > 0.005 {
            warn!(
                "[BOOK AUDIT] SIGNIFICANT DIVERGENCE on {id_short}: \
                 yes_ask WS={ws_yes_ask:.4} REST={rest_yes_ask:.4} | \
                 no_ask WS={ws_no_ask:.4} REST={rest_no_ask:.4}"
            );
        }

        // Also log midpoints for comparison with website display
        let ws_yes_mid = (ws_yes_ask + ws_yes_bid) / 2.0;
        let ws_no_mid = (ws_no_ask + ws_no_bid) / 2.0;
        let rest_yes_mid = (rest_yes_ask + rest_yes_bid) / 2.0;
        let rest_no_mid = (rest_no_ask + rest_no_bid) / 2.0;
        info!(
            "[BOOK AUDIT] {id_short} midpoints | \
             WS: yes_mid={ws_yes_mid:.4} no_mid={ws_no_mid:.4} total={:.4} | \
             REST: yes_mid={rest_yes_mid:.4} no_mid={rest_no_mid:.4} total={:.4}",
            ws_yes_mid + ws_no_mid,
            rest_yes_mid + rest_no_mid,
        );
    }
}

/// Fetch the CLOB REST order book for a single token and return `(best_ask, best_bid)`.
async fn fetch_rest_best_prices(client: &reqwest::Client, token_id: &str) -> Option<(f64, f64)> {
    let url = format!("{}/book?token_id={}", CLOB_API_URL, token_id);
    let resp: serde_json::Value = match client.get(&url).send().await {
        Ok(r) => match r.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!("[BOOK AUDIT] JSON parse error for {}: {e}", &token_id[..10.min(token_id.len())]);
                return None;
            }
        },
        Err(e) => {
            warn!("[BOOK AUDIT] HTTP error for {}: {e}", &token_id[..10.min(token_id.len())]);
            return None;
        }
    };

    let best_ask = resp
        .get("asks")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|level| level.get("price"))
        .and_then(|p| p.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);

    let best_bid = resp
        .get("bids")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|level| level.get("price"))
        .and_then(|p| p.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);

    if best_ask > 0.0 {
        Some((best_ask, best_bid))
    } else {
        None
    }
}

/// Parse an RFC 3339 / ISO 8601 UTC timestamp string to Unix seconds.
pub fn parse_iso_to_unix_secs(s: &str) -> Option<f64> {
    let s = s.trim();
    let s = s.strip_suffix('Z').unwrap_or(s);
    let s = if let Some(dot) = s.find('.') { &s[..dot] } else { s };
    let s = if s.len() > 19 {
        let b = s.as_bytes()[19];
        if b == b'+' || b == b'-' { &s[..19] } else { s }
    } else {
        s
    };
    let (year, month, day, hour, minute, second): (i64, i64, i64, i64, i64, i64) =
        if s.len() == 10 {
            (s[0..4].parse().ok()?, s[5..7].parse().ok()?, s[8..10].parse().ok()?, 0, 0, 0)
        } else if s.len() >= 19 {
            (s[0..4].parse().ok()?, s[5..7].parse().ok()?, s[8..10].parse().ok()?,
             s[11..13].parse().ok()?, s[14..16].parse().ok()?, s[17..19].parse().ok()?)
        } else {
            return None;
        };

    let a = (14 - month) / 12;
    let y = year + 4800 - a;
    let m = month + 12 * a - 3;
    let jdn = day + (153 * m + 2) / 5 + 365 * y + y / 4 - y / 100 + y / 400 - 32045;
    let unix_days = jdn - 2_440_588;
    Some((unix_days * 86400 + hour * 3600 + minute * 60 + second) as f64)
}

pub fn parse_category_str(s: &str) -> Category {
    match s.to_lowercase().as_str() {
        "sports" => Category::Sports,
        "weather" => Category::Weather,
        "crypto" => Category::Crypto,
        "politics" => Category::Politics,
        "economics" => Category::Economics,
        _ => Category::Other,
    }
}
