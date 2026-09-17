-- A job outlives a bank account, and a salary outlives a payroll memo.
--
-- `income_streams` has carried the matcher's target as one pair of columns since 0027:
-- `match_account_id` plus `match_pattern`, both set or matching is off. One account, one
-- substring. That is right for as long as nothing changes, and three ordinary things change it:
--
--   * **the bank account changes.** Salary paid into a chequing account for two years starts
--     landing in an offset/revolving facility instead. Same job, same employer, same memo.
--   * **the memo changes.** A bank rewrites its statement format, or the employer moves payroll
--     provider. In the tree this was found in, ASB's own memo for one employer went from
--     `D/C FROM ACME GROUP LTD SALARY/WAGES PAY ENDED 12-AUG-2025` to
--     `ACME GROUP LIM SALARY/WAGESPAY ENDED12-AUG-2025` — the prefix dropped, `LTD` became
--     `LIM`, and the space inside `WAGES PAY` closed up. No single substring spans both.
--   * **both at once**, which is what a bank switch usually is.
--
-- With one pair, every deposit before the change is invisible to the matcher: not "unmatched"
-- but never considered, because the candidate query filters on the one account before it looks
-- at anything else. The history then reads as years of missed pays, and the cash-flow chart
-- draws a gross-pay layer over the months since the switch and bare category totals before it.
-- The tree this was written for had years of one salary in exactly that state — every deposit
-- before the bank switch unmatchable, and no way to say so in the configuration.
--
-- The alternative was a second stream per era, and it is worse than it sounds: two streams for
-- one job double-count in `person_regular_annualised` (which sums a person's regular gross to
-- find the bracket an extra pay starts in), split one pay scale across two schedules, and show
-- up in the UI as two jobs. A target is not a stream. It is a place to look.
--
-- Rows, not columns, because the number of eras is not knowable in advance — this stream needs
-- four and the next one needs one.

CREATE TABLE income_stream_match_targets (
    id               INTEGER PRIMARY KEY,
    income_stream_id INTEGER NOT NULL REFERENCES income_streams(id) ON DELETE CASCADE,
    -- ON DELETE CASCADE rather than the `SET NULL` the old column had: half a target is not a
    -- target, and a NULL account would read as "look everywhere" to a future writer.
    account_id       INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    -- Stored as written; compared case-insensitively. Non-empty by CHECK, because a blank
    -- pattern is a substring of every description and would claim the first deposit in range.
    pattern          TEXT NOT NULL CHECK (length(trim(pattern)) > 0),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- Carry every configured pair over. The `trim`/`<> ''` guard mirrors what `sure-dal` has always
-- written (trimmed, empty as NULL) and what `match_key` demanded when reading, so a row with a
-- whitespace pattern — matching off in practice — does not become a target that matches
-- everything.
INSERT INTO income_stream_match_targets (income_stream_id, account_id, pattern)
SELECT id, match_account_id, trim(match_pattern)
  FROM income_streams
 WHERE match_account_id IS NOT NULL
   AND trim(coalesce(match_pattern, '')) <> '';

-- One target per (stream, account, pattern): a duplicate is a typo, and two identical targets
-- would put the same stream in one group twice.
CREATE UNIQUE INDEX idx_income_stream_match_targets_unique
    ON income_stream_match_targets(income_stream_id, account_id, pattern);
-- The matcher reads every target for a stream, and the account list for a deleted account.
CREATE INDEX idx_income_stream_match_targets_stream
    ON income_stream_match_targets(income_stream_id);
CREATE INDEX idx_income_stream_match_targets_account
    ON income_stream_match_targets(account_id);

-- Dropped rather than left in place: two sources of truth for "where does this land" is how the
-- next reader ends up matching on the stale one. Neither column is indexed or named in a CHECK,
-- so SQLite can drop them without a table rebuild.
ALTER TABLE income_streams DROP COLUMN match_account_id;
ALTER TABLE income_streams DROP COLUMN match_pattern;
