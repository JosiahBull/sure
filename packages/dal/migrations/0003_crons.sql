-- Scheduled adjustments ("crons"): recurring valuation changes (appreciation,
-- depreciation, interest) or recurring transactions, applied monthly. Each applied
-- period is recorded so runs are idempotent and reversible.

CREATE TABLE crons (
    id            INTEGER PRIMARY KEY,
    name          TEXT NOT NULL,
    account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    -- 'appreciation' | 'depreciation' | 'interest' | 'fixed_transaction'
    kind          TEXT NOT NULL,
    -- Annual rate in basis points (100 = 1%/yr) for the valuation kinds.
    rate_bps      INTEGER,
    -- Signed minor units for the fixed_transaction kind.
    amount_minor  INTEGER,
    category_id   INTEGER REFERENCES categories(id) ON DELETE SET NULL,
    frequency     TEXT NOT NULL DEFAULT 'monthly',
    day_of_month  INTEGER NOT NULL DEFAULT 1,
    start_date    TEXT NOT NULL,
    last_run_on   TEXT,                  -- last period date applied
    enabled       INTEGER NOT NULL DEFAULT 1,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- One applied period, pointing at the artifact it produced so it can be undone.
CREATE TABLE cron_runs (
    id             INTEGER PRIMARY KEY,
    cron_id        INTEGER NOT NULL REFERENCES crons(id) ON DELETE CASCADE,
    period         TEXT NOT NULL,        -- the period date applied
    kind           TEXT NOT NULL,
    valuation_id   INTEGER REFERENCES valuations(id) ON DELETE SET NULL,
    transaction_id INTEGER REFERENCES transactions(id) ON DELETE SET NULL,
    detail         TEXT,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX idx_cron_runs_cron ON cron_runs(cron_id);
CREATE UNIQUE INDEX idx_cron_runs_period ON cron_runs(cron_id, period);
