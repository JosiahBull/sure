-- Equity compensation: multiple grants (across one or more companies) attached to a
-- shares_private account, with standard cliff + linear-monthly vesting, and a log of
-- option exercises. Vested/unvested/exercised are computed on demand from these rows.

CREATE TABLE equity_grants (
    id                INTEGER PRIMARY KEY,
    account_id        INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    company           TEXT NOT NULL,
    grant_date        TEXT NOT NULL,
    quantity          INTEGER NOT NULL,            -- total units granted
    strike_minor      INTEGER NOT NULL DEFAULT 0,  -- strike per unit (0 => RSU)
    currency_code     TEXT NOT NULL REFERENCES currencies(code),
    vest_months       INTEGER NOT NULL DEFAULT 48,
    cliff_months      INTEGER NOT NULL DEFAULT 12,
    unit_value_minor  INTEGER,                     -- latest fair value per unit
    note              TEXT,
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX idx_grants_account ON equity_grants(account_id);

CREATE TABLE equity_exercises (
    id            INTEGER PRIMARY KEY,
    grant_id      INTEGER NOT NULL REFERENCES equity_grants(id) ON DELETE CASCADE,
    exercise_date TEXT NOT NULL,
    quantity      INTEGER NOT NULL,
    price_minor   INTEGER NOT NULL DEFAULT 0,      -- price paid per unit
    note          TEXT,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX idx_exercises_grant ON equity_exercises(grant_id);
