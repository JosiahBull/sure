-- What a file import did, one row per (upload, account).
--
-- The counterpart to `provider_syncs` (0005), for uploads rather than feeds, and for the same
-- reason: without a record, "how much of this account came from an export, and how far back does
-- it reach?" could only be answered by reading the transactions themselves. Two UI panels did
-- exactly that — fetching up to 10,000 rows and filtering client-side on the provider tag — which
-- is a lot of work to arrive at a number the import already knew.
--
-- Deliberately *not* a handle for undo. Two overlapping uploads of the same window share their
-- content-derived external ids, so the second one's rows were skipped rather than written and
-- there is nothing of it left to take back; undo is per (account, source), which is what the
-- provider tag on each row already expresses. This table records what happened, and does not
-- claim any of it is individually reversible.
--
-- Like every other audit table here it is cleared but not restored by a snapshot import (see
-- `dal/src/snapshot.rs`): it is a log of actions taken against a database, not data belonging to
-- the household, and re-inserting one database's log into another would be a false history.
CREATE TABLE imports (
    id             INTEGER PRIMARY KEY,
    account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    -- `sure_core::ImportSource`, parsed into the enum on the way out (CLAUDE.md rule 1).
    source         TEXT NOT NULL,              -- 'asb_csv' | 'myir_sls' | 'sharesies_zip' | 'csv_upload'
    -- The tag the rows themselves carry (`asb#12`), so a row here can be tied back to them.
    provider_tag   TEXT NOT NULL,
    -- How the *source* named what was imported: an ASB account number, an SLS account id. Null
    -- for a source whose files name nothing.
    source_account TEXT,
    -- The file names inside the upload, as a JSON array. Display only — which downloads these
    -- were is the one thing neither the rows nor the window can tell you afterwards.
    filenames      TEXT NOT NULL DEFAULT '[]',
    imported       INTEGER NOT NULL DEFAULT 0,
    skipped        INTEGER NOT NULL DEFAULT 0,
    held_back      INTEGER NOT NULL DEFAULT 0,
    covered_from   TEXT,
    covered_to     TEXT,
    cutover        TEXT,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
-- Newest first per account: the only read this table has.
CREATE INDEX idx_imports_account ON imports(account_id, created_at DESC);
