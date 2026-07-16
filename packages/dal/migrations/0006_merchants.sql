-- First-class merchants: a custom, reusable payee with an optional default category.
-- Transactions keep their raw imported `merchant` text and additionally reference a
-- resolved merchant via `merchant_id`. Rules can assign a merchant, so the audit log
-- gains before/after merchant columns for undo.

CREATE TABLE merchants (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    category_id INTEGER REFERENCES categories(id) ON DELETE SET NULL, -- suggested category
    note        TEXT,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
-- Custom merchants are unique by name, case-insensitively.
CREATE UNIQUE INDEX idx_merchants_name ON merchants(name COLLATE NOCASE);

ALTER TABLE transactions ADD COLUMN merchant_id INTEGER REFERENCES merchants(id) ON DELETE SET NULL;
CREATE INDEX idx_tx_merchant ON transactions(merchant_id);

ALTER TABLE rules ADD COLUMN set_merchant_id INTEGER REFERENCES merchants(id) ON DELETE SET NULL;

ALTER TABLE rule_applications ADD COLUMN prev_merchant_id INTEGER;
ALTER TABLE rule_applications ADD COLUMN new_merchant_id INTEGER;
