-- At most one provider-sourced valuation per account per day: a provider's transaction
-- history is often incomplete (e.g. a mortgage's history rarely reaches back to when the
-- loan originated), so summed transactions alone drift from the real balance. Providers
-- that can report a live balance (see `TransactionProvider::current_balance`) refresh a
-- same-day valuation on every sync instead — this partial unique index lets that be a
-- true upsert (repeated same-day syncs update in place rather than accumulating rows).
-- Manual/cron-sourced valuations are untouched by this constraint.
CREATE UNIQUE INDEX idx_valuations_provider_daily
    ON valuations(account_id, as_of) WHERE source = 'provider';
