-- Historical daily closing prices per (ticker, exchange), fetched from an external
-- StockPriceProvider (see sure-providers). Unlike exchange_rate_cache (a latest-only
-- cache), this *is* the historical series — a point-in-time lookup already needs to
-- query by date, so there's no separate "latest" table.
CREATE TABLE stock_prices (
    ticker        TEXT NOT NULL,
    exchange      TEXT NOT NULL DEFAULT '',
    as_of         TEXT NOT NULL,                     -- ISO-8601 date, daily resolution
    close         TEXT NOT NULL,                     -- decimal-as-text, exact
    currency_code TEXT NOT NULL REFERENCES currencies(code),
    fetched_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (ticker, exchange, as_of)
) STRICT;
