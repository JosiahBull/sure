-- At most one equity-sourced valuation per account per day, so revaluing the same date twice
-- refreshes the row instead of stacking another one beside it.
--
-- The last of the four derived sources to get this; `provider` (0010), `brokerage` (0012) and
-- `estimate` (0036) have had it all along, and `equity` was the odd one out purely because
-- nothing revalued in bulk. Rebuilding an account's whole valuation history from its grant
-- schedule and mark ledger writes one row per month, which makes the gap load-bearing: without
-- this, a second rebuild doubles the series and the net-worth chart reads whichever duplicate
-- sorts first. `manual`/`cron` valuations are untouched by this constraint.

-- Collapse any existing duplicates first, keeping the most recently written of each day — the
-- index cannot be created over them, and on the same date the newest row is the one that
-- reflects the current grants. Cheap on a real database: there is at most one equity account.
DELETE FROM valuations
 WHERE source = 'equity'
   AND id NOT IN (SELECT MAX(id) FROM valuations WHERE source = 'equity'
                   GROUP BY account_id, as_of);

CREATE UNIQUE INDEX idx_valuations_equity_daily
    ON valuations(account_id, as_of) WHERE source = 'equity';
