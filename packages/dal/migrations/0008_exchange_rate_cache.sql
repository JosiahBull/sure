-- Cache of the most recently polled exchange rate per currency pair (see the
-- background exchange-rate poller in sure-api). One row per pair, continuously
-- overwritten — unlike `exchange_rates` above, which keeps dated historical
-- snapshots for reports.
CREATE TABLE exchange_rate_cache (
    base_code   TEXT NOT NULL REFERENCES currencies(code),
    quote_code  TEXT NOT NULL REFERENCES currencies(code),
    rate        TEXT NOT NULL,
    as_of       TEXT NOT NULL,                     -- upstream's reference date
    fetched_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (base_code, quote_code)
) STRICT;
