-- At most one estimate-sourced valuation per account per day, so a re-poll on the same day
-- refreshes the row in place rather than accumulating one per attempt. Mirrors
-- 0010_provider_valuations.sql and 0012_brokerage.sql, and — like them — gets its own source
-- rather than sharing `provider`: a property account can be both balance-linked and subscribed
-- to House Pricer, and two upserts sharing one predicate would overwrite each other's row on
-- every poll. `manual`/`cron` valuations are untouched by this constraint.
CREATE UNIQUE INDEX idx_valuations_estimate_daily
    ON valuations(account_id, as_of) WHERE source = 'estimate';
