-- Core schema: currencies, accounts, categories, transactions, valuations, fx rates.
-- Conventions:
--   * Money is stored as signed INTEGER minor units (e.g. cents); negative = outflow.
--   * Dates/timestamps are ISO-8601 TEXT (UTC) so they round-trip cleanly to JSON.
--   * Booleans are 0/1 INTEGER.

CREATE TABLE currencies (
    code            TEXT PRIMARY KEY,               -- e.g. 'NZD', 'USD'
    name            TEXT NOT NULL,
    symbol          TEXT NOT NULL,
    decimal_places  INTEGER NOT NULL DEFAULT 2,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- Single-row global settings (base reporting currency, etc.).
CREATE TABLE settings (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    base_currency_code  TEXT NOT NULL REFERENCES currencies(code),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- Accounts. `kind` drives type-specific behaviour; `metadata` holds kind-specific
-- configuration as a JSON object, validated in Rust via a tagged enum.
CREATE TABLE accounts (
    id              INTEGER PRIMARY KEY,
    name            TEXT NOT NULL,
    kind            TEXT NOT NULL,                  -- see AccountKind (Rust)
    currency_code   TEXT NOT NULL REFERENCES currencies(code),
    institution     TEXT,
    metadata        TEXT NOT NULL DEFAULT '{}',     -- JSON object
    archived        INTEGER NOT NULL DEFAULT 0,
    sort_order      INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX idx_accounts_kind ON accounts(kind);

-- Categories with self-referential nesting. `kind` classifies flow direction so the
-- reports can split income vs expense; 'transfer' is excluded from spend reports.
CREATE TABLE categories (
    id              INTEGER PRIMARY KEY,
    name            TEXT NOT NULL,
    parent_id       INTEGER REFERENCES categories(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL DEFAULT 'expense', -- 'income' | 'expense' | 'transfer'
    color           TEXT,
    icon            TEXT,
    sort_order      INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX idx_categories_parent ON categories(parent_id);

-- Transactions. `amount_minor` is signed minor units in `currency_code`
-- (negative = outflow). Transfers are two transactions linked reciprocally.
CREATE TABLE transactions (
    id                      INTEGER PRIMARY KEY,
    account_id              INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    posted_at               TEXT NOT NULL,          -- ISO-8601
    amount_minor            INTEGER NOT NULL,
    currency_code           TEXT NOT NULL REFERENCES currencies(code),
    description             TEXT NOT NULL DEFAULT '',
    merchant                TEXT,
    notes                   TEXT,
    category_id             INTEGER REFERENCES categories(id) ON DELETE SET NULL,
    is_one_off              INTEGER NOT NULL DEFAULT 0,   -- excluded from regular reports
    linked_transaction_id   INTEGER REFERENCES transactions(id) ON DELETE SET NULL,
    provider                TEXT,                   -- provenance for imports
    external_id             TEXT,
    categorized_by_rule_id  INTEGER,                -- which rule last set the category
    created_at              TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at              TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX idx_tx_account ON transactions(account_id);
CREATE INDEX idx_tx_posted ON transactions(posted_at);
CREATE INDEX idx_tx_category ON transactions(category_id);
-- Imported transactions dedupe on (provider, external_id).
CREATE UNIQUE INDEX idx_tx_provider_external ON transactions(provider, external_id)
    WHERE provider IS NOT NULL AND external_id IS NOT NULL;

-- Point-in-time valuations for asset/liability accounts (house, vehicle, shares,
-- loan balances). Net-worth history is derived from these plus cash-account flows.
CREATE TABLE valuations (
    id              INTEGER PRIMARY KEY,
    account_id      INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    as_of           TEXT NOT NULL,                  -- ISO-8601 date
    value_minor     INTEGER NOT NULL,               -- signed; liabilities are negative
    currency_code   TEXT NOT NULL REFERENCES currencies(code),
    source          TEXT NOT NULL DEFAULT 'manual', -- 'manual' | 'cron' | 'provider'
    note            TEXT,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX idx_valuations_account ON valuations(account_id, as_of);

-- Exchange rates for normalising multi-currency figures into the base currency.
-- Stored as a decimal string to avoid binary-float drift: 1 base = <rate> quote.
CREATE TABLE exchange_rates (
    base_code       TEXT NOT NULL REFERENCES currencies(code),
    quote_code      TEXT NOT NULL REFERENCES currencies(code),
    as_of           TEXT NOT NULL,                  -- ISO-8601 date
    rate            TEXT NOT NULL,
    PRIMARY KEY (base_code, quote_code, as_of)
) STRICT;

-- Seed common currencies for an NZ-based household and default the base to NZD.
INSERT OR IGNORE INTO currencies (code, name, symbol, decimal_places) VALUES
    ('NZD', 'New Zealand Dollar', '$',  2),
    ('USD', 'US Dollar',          '$',  2),
    ('AUD', 'Australian Dollar',  '$',  2),
    ('GBP', 'Pound Sterling',     '£',  2),
    ('EUR', 'Euro',               '€',  2);

INSERT OR IGNORE INTO settings (id, base_currency_code) VALUES (1, 'NZD');
