-- Drop the forecast tables. The engine that read them is gone; these are the rows it left.
--
-- Numbered 0047, not 0041, even though 0041-0046 are absent from this tree. Those six numbers
-- were used by the forecast-modelling PRs that were closed unmerged (#40, #41), and a database
-- that ran either branch has them recorded in `_sqlx_migrations` — reusing 0041 gives that
-- database two different migrations under one version, which is a checksum conflict on top of
-- the "previously applied but missing" error those rows already cause. Starting above the
-- highest number those branches reached costs nothing and keeps the two histories disjoint.
--
-- This destroys data, and there is no way back: the assumptions somebody typed, the life events
-- they dated, and the effects and orderings hung off those events. Nothing exports them any more
-- either, so a config snapshot taken after the engine was removed does not carry them and cannot
-- restore them. That is the intent — the alternative was four tables nothing reads, growing
-- stale behind a feature that no longer exists.
--
-- Children before parents. `forecast_event_effects` and `forecast_event_relations` both hold
-- `REFERENCES forecast_events(id)`, and `forecast_event_relations` holds a second one
-- (`depends_on_event_id`) with no `ON DELETE` clause at all, so dropping the parent first is
-- refused outright when foreign keys are enforced. SQLite drops each table's indexes with it, so
-- the six `idx_forecast_*` indexes need no statement of their own.
--
-- This also retires the guard in `snapshot.rs`'s restore wipe, removed in the same commit: it
-- cleared `forecast_events` ahead of `income_streams` because
-- `forecast_event_effects.income_stream_id` is `ON DELETE RESTRICT`, and a leftover effect row
-- would otherwise refuse the restore's `DELETE FROM income_streams`. With the tables gone the
-- restriction goes with them, which is the honest fix rather than a delete that outlives its
-- reason.
DROP TABLE IF EXISTS forecast_event_relations;
DROP TABLE IF EXISTS forecast_event_effects;
DROP TABLE IF EXISTS forecast_events;
DROP TABLE IF EXISTS forecast_assumptions;

-- And the two the abandoned branches added, which no migration in this tree ever created and so
-- no migration in this tree would otherwise remove. `IF EXISTS` is doing real work here: on any
-- database that never ran #40/#41 these are no-ops, and on one that did they are the last of the
-- forecast schema. Both are forecast features — stated spending commitments, and what the
-- simulation did with money it did not spend — so they go for the same reason as the four above.
DROP TABLE IF EXISTS expense_commitments;
DROP TABLE IF EXISTS investment_strategies;
