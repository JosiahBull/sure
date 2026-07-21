-- Forecast: tunable growth/volatility/yield assumptions plus known future step-changes,
-- used to project net worth/cash flow forward. Every assumption field defaults to
-- "derive from history" (NULL) but can be overridden per account or per category; an
-- explicit forecast_events row is a user-asserted certainty (a promotion, a planned
-- one-off), not a statistical estimate, so it applies identically to every simulated path.
--
-- Nothing here is written to by the simulation itself — a forecast is computed in memory
-- from a snapshot of current state and never touches `transactions`/`valuations`, unlike
-- the `crons` engine (0003_crons.sql), which persists real rows when run.

CREATE TABLE forecast_assumptions (
    id                      INTEGER PRIMARY KEY,
    target_type             TEXT NOT NULL CHECK (target_type IN ('account', 'category')),
    target_id               INTEGER NOT NULL,
    -- Annual growth rate in basis points (100 = 1%/yr). NULL = derive from history:
    -- CAGR of an account's valuation series, or the linear trend of a category's monthly
    -- totals.
    annual_growth_bps       INTEGER,
    -- Annual volatility (return/residual standard deviation) in basis points, for the
    -- Monte Carlo simulation's noise term. NULL = derive from history.
    annual_volatility_bps   INTEGER,
    -- Expected annual dividend yield in basis points. Account-only (meaningless for a
    -- category); NULL = derive from trailing 12 months of `dividends`.
    dividend_yield_bps      INTEGER,
    notes                   TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE UNIQUE INDEX idx_forecast_assumptions_target
    ON forecast_assumptions(target_type, target_id);

-- Known future step-changes: a promotion (a new recurring baseline from a date), or a
-- planned one-off inflow/outflow (a bonus, a planned large purchase). Applied uniformly
-- across every simulated path — these are certainties the user is asserting, not
-- estimates the simulation should add noise to.
CREATE TABLE forecast_events (
    id              INTEGER PRIMARY KEY,
    target_type     TEXT NOT NULL CHECK (target_type IN ('account', 'category')),
    target_id       INTEGER NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('step_change', 'one_off_amount')),
    effective_date  TEXT NOT NULL,
    -- step_change: the new recurring monthly baseline from effective_date on (replaces
    -- the derived/overridden trend baseline for a category, or a valuation jump for an
    -- account). one_off_amount: a signed one-time amount applied on effective_date only.
    amount_minor    INTEGER NOT NULL,
    label           TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX idx_forecast_events_target ON forecast_events(target_type, target_id);
