CREATE TABLE IF NOT EXISTS markets (
    id TEXT PRIMARY KEY,
    question TEXT NOT NULL,
    category TEXT,
    end_date_iso TEXT,
    total_volume REAL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS windows (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    market_id TEXT NOT NULL,
    opened_at INTEGER NOT NULL,
    closed_at INTEGER,
    duration_ms REAL,
    yes_ask REAL NOT NULL,
    no_ask REAL NOT NULL,
    combined_cost REAL NOT NULL,
    spread_size REAL NOT NULL,
    spread_category TEXT,

    -- Dimension 1: persistence
    open_duration_class TEXT,

    -- Dimension 2: close reason (multi_tick only)
    close_reason TEXT,

    -- Raw observables (stored separately so classification can improve without re-scanning)
    tick_count INTEGER DEFAULT 1,
    volume_changed INTEGER DEFAULT 0,
    volume_change_ticks INTEGER,
    price_shifted INTEGER DEFAULT 0,

    -- Composite priority score
    opportunity_class INTEGER
);

CREATE INDEX IF NOT EXISTS idx_windows_market_id ON windows(market_id);
CREATE INDEX IF NOT EXISTS idx_windows_opened_at ON windows(opened_at);

CREATE TABLE IF NOT EXISTS market_stats (
    market_id TEXT PRIMARY KEY,
    windows_24h INTEGER DEFAULT 0,
    avg_window_duration_ms REAL,
    avg_spread_size REAL,
    max_spread_size REAL,
    noise_ratio REAL,
    opportunity_score REAL,
    last_updated INTEGER
);
