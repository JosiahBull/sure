-- Brokerage accounts: many stock/fund holdings + per-currency cash wallets under one
-- account, driven by a bulk import (see sure-providers::sharesies). Current value is
-- computed lazily (holdings x latest price + wallet cash), not stored directly here.

-- Buy/sell/corporate-action lots. Quantity is REAL (fractional shares; share counts
-- aren't money, so this matches the rest of the app's float-based aggregation — see
-- routes::reports::Fx — rather than the decimal-as-text convention used for prices).
CREATE TABLE holdings (
    id            INTEGER PRIMARY KEY,
    account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    ticker        TEXT NOT NULL,
    exchange      TEXT NOT NULL DEFAULT '',
    name          TEXT,
    currency_code TEXT NOT NULL REFERENCES currencies(code),
    trade_date    TEXT NOT NULL,
    quantity      REAL NOT NULL,          -- signed: +buy/corporate credit, -sell
    unit_price    REAL,                   -- informational only
    fee_minor     INTEGER NOT NULL DEFAULT 0,
    kind          TEXT NOT NULL,          -- 'buy' | 'sell' | 'corporate'
    external_id   TEXT,
    provider      TEXT,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX idx_holdings_account ON holdings(account_id, trade_date);
CREATE UNIQUE INDEX idx_holdings_provider_external ON holdings(provider, external_id)
    WHERE provider IS NOT NULL AND external_id IS NOT NULL;

-- Dividend/distribution detail (supplementary — the net cash impact is already an
-- ordinary imported transaction; this is for future tax reporting).
CREATE TABLE dividends (
    id                 INTEGER PRIMARY KEY,
    account_id         INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    ticker             TEXT NOT NULL,
    exchange           TEXT NOT NULL DEFAULT '',
    record_date        TEXT,
    paid_date          TEXT NOT NULL,
    shares_held        REAL,
    gross_amount_minor INTEGER NOT NULL,
    net_amount_minor   INTEGER NOT NULL,
    currency_code      TEXT NOT NULL REFERENCES currencies(code),
    external_id        TEXT,
    provider           TEXT,
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE UNIQUE INDEX idx_dividends_provider_external ON dividends(provider, external_id)
    WHERE provider IS NOT NULL AND external_id IS NOT NULL;

CREATE TABLE dividend_withholdings (
    id               INTEGER PRIMARY KEY,
    dividend_id      INTEGER NOT NULL REFERENCES dividends(id) ON DELETE CASCADE,
    owed_to          TEXT NOT NULL,       -- e.g. 'NZ_IRD', 'US_IRS'
    tax_amount_minor INTEGER NOT NULL,
    tax_credit_minor INTEGER,
    currency_code    TEXT NOT NULL REFERENCES currencies(code)
) STRICT;
CREATE INDEX idx_dividend_withholdings_dividend ON dividend_withholdings(dividend_id);

-- Mirrors 0010_provider_valuations.sql's per-day upsert trick, but with its own source
-- tag so it never collides with an unrelated `source='provider'` sync on the same account
-- (e.g. if an Akahu balance-only link is still attached).
CREATE UNIQUE INDEX idx_valuations_brokerage_daily
    ON valuations(account_id, as_of) WHERE source = 'brokerage';
