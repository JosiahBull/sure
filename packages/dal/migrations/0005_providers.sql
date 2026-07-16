-- External provider connections and their sync history. Imported transactions land
-- in `transactions` tagged with `provider = '<kind>#<connection id>'` and dedupe on
-- (provider, external_id) via the unique index from the core migration.

CREATE TABLE providers (
    id             INTEGER PRIMARY KEY,
    name           TEXT NOT NULL,
    kind           TEXT NOT NULL,           -- selects the TransactionProvider impl
    account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    config         TEXT NOT NULL DEFAULT '{}',  -- JSON, provider-specific
    enabled        INTEGER NOT NULL DEFAULT 1,
    last_synced_at TEXT,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE provider_syncs (
    id          INTEGER PRIMARY KEY,
    provider_id INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    imported    INTEGER NOT NULL DEFAULT 0,
    skipped     INTEGER NOT NULL DEFAULT 0,
    status      TEXT NOT NULL,              -- 'ok' | 'error'
    detail      TEXT,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX idx_provider_syncs_provider ON provider_syncs(provider_id);
