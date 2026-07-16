-- Auto-classification rules, their run history, and a per-transaction audit log
-- that makes every run reversible (undo) and repeatable (rerun).

CREATE TABLE rules (
    id               INTEGER PRIMARY KEY,
    name             TEXT NOT NULL,
    description      TEXT,
    -- A Zen expression evaluated against a transaction context; truthy => match.
    expression       TEXT NOT NULL,
    -- Actions applied on match:
    set_category_id  INTEGER REFERENCES categories(id) ON DELETE SET NULL,
    set_one_off      INTEGER,               -- nullable boolean; NULL => leave unchanged
    -- When 0, the rule won't overwrite a manually-set category.
    overwrite_manual INTEGER NOT NULL DEFAULT 0,
    -- When 1, later (lower-priority) rules are skipped for a matched transaction.
    stop_on_match    INTEGER NOT NULL DEFAULT 0,
    priority         INTEGER NOT NULL DEFAULT 0,   -- lower runs first
    enabled          INTEGER NOT NULL DEFAULT 1,
    created_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX idx_rules_priority ON rules(priority);

-- One execution over the transaction set (a single rule or all rules).
CREATE TABLE rule_runs (
    id           INTEGER PRIMARY KEY,
    rule_id      INTEGER REFERENCES rules(id) ON DELETE SET NULL,  -- NULL => "run all"
    kind         TEXT NOT NULL,           -- 'single' | 'all'
    matched      INTEGER NOT NULL DEFAULT 0,
    changed      INTEGER NOT NULL DEFAULT 0,
    undone       INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- Audit log: one row per transaction actually changed by a run, capturing the
-- prior state so the run can be undone precisely.
CREATE TABLE rule_applications (
    id                          INTEGER PRIMARY KEY,
    rule_run_id                 INTEGER NOT NULL REFERENCES rule_runs(id) ON DELETE CASCADE,
    rule_id                     INTEGER REFERENCES rules(id) ON DELETE SET NULL,
    transaction_id              INTEGER NOT NULL REFERENCES transactions(id) ON DELETE CASCADE,
    prev_category_id            INTEGER,
    new_category_id             INTEGER,
    prev_categorized_by_rule_id INTEGER,
    prev_one_off                INTEGER,
    new_one_off                 INTEGER,
    reverted                    INTEGER NOT NULL DEFAULT 0,
    created_at                  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX idx_rule_apps_run ON rule_applications(rule_run_id);
CREATE INDEX idx_rule_apps_tx ON rule_applications(transaction_id);
