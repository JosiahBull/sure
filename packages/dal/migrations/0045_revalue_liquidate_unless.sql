-- Three primitives a real scenario needed and the event model could not express.
--
-- The scenario: a private holding has a 40% chance of going to zero within a year; if it does not,
-- a funding round re-prices it 6x at eighteen months, half the vested units are sold for cash, and
-- a later round re-prices what is left again. Every part of that was inexpressible:
--
--   1. **"if it does not"** — `RelationKind` had `only_if` but no negation, so two complementary
--      outcomes could only be modelled as two independent draws. At 40% and 60% that means 24% of
--      paths get *both* the wipe-out and the funding round, and 24% get neither. `only_if_not` is
--      the missing complement, and with it a 40% failure event plus an `only_if_not` chain is a
--      genuine partition of the paths.
--   2. **a re-mark** — `set_baseline` sets an account's *value*, which is the wrong quantity for a
--      holding projected as `units x price`: it would overwrite the vesting ramp with a constant
--      and the units still to arrive would stop arriving. A funding round changes the price, not
--      the quantity. `revalue` multiplies the price factor and leaves the ramp alone, which is
--      exactly the separation `AccountProjection::Vesting` exists to maintain. A factor of zero is
--      how the company failing is written.
--   3. **a partial sale** — nothing moved value out of an account and into spendable cash. Selling
--      half a holding is not a `set_baseline` to half (that discards the proceeds) and not a
--      `one_off_amount` (that invents money). `liquidate` reduces the account and credits the cash
--      pool with what came out, which is the only shape that conserves it.
--
-- Both tables are rebuilt because SQLite cannot alter a CHECK in place. They are safe to drop and
-- recreate — unlike 0038's `income_streams`, nothing REFERENCES either of them, so the
-- `PRAGMA foreign_keys = ON` that a migration cannot turn off has nothing to cascade to. Verified
-- by grep over every migration before writing this.

CREATE TABLE forecast_event_effects_new (
    id                INTEGER PRIMARY KEY,
    event_id          INTEGER NOT NULL REFERENCES forecast_events(id) ON DELETE CASCADE,
    kind              TEXT NOT NULL CHECK (kind IN (
                          'income_step',
                          'income_start',
                          'income_end',
                          'income_pause',
                          'recurring_delta',
                          'set_baseline',
                          'one_off_amount',
                          -- Multiply an account's price by `rate_bps`. 60000 = a 6x round,
                          -- 0 = the company failed.
                          'revalue',
                          -- Sell `rate_bps` of an account into cash. 5000 = half.
                          'liquidate')),
    sort_order        INTEGER NOT NULL DEFAULT 0,
    income_stream_id  INTEGER REFERENCES income_streams(id) ON DELETE RESTRICT,
    person_id         INTEGER REFERENCES people(id)         ON DELETE RESTRICT,
    category_id       INTEGER REFERENCES categories(id)     ON DELETE RESTRICT,
    account_id        INTEGER REFERENCES accounts(id)       ON DELETE RESTRICT,
    amount_minor      INTEGER,
    rate_bps          INTEGER,
    delay_months      INTEGER,
    ramp_months       INTEGER,
    duration_months   INTEGER,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    -- Carried across verbatim, plus the two new arms. The `ELSE 0` is still load-bearing for the
    -- same reason: a kind added above and forgotten here would return NULL, and a CHECK passes on
    -- NULL, so the new kind would silently accept any shape at all.
    CHECK (CASE kind
        WHEN 'income_step' THEN income_stream_id IS NOT NULL
             AND person_id IS NULL AND category_id IS NULL AND account_id IS NULL
             AND ((amount_minor IS NOT NULL) <> (rate_bps IS NOT NULL))
             AND delay_months IS NULL AND ramp_months IS NULL AND duration_months IS NULL
        WHEN 'income_start' THEN income_stream_id IS NOT NULL
             AND person_id IS NULL AND category_id IS NULL AND account_id IS NULL
             AND amount_minor IS NULL AND rate_bps IS NULL
             AND delay_months IS NULL AND ramp_months IS NULL AND duration_months IS NULL
        WHEN 'income_end' THEN income_stream_id IS NOT NULL
             AND person_id IS NULL AND category_id IS NULL AND account_id IS NULL
             AND amount_minor IS NULL AND rate_bps IS NULL
             AND delay_months IS NULL AND ramp_months IS NULL AND duration_months IS NULL
        WHEN 'income_pause' THEN person_id IS NOT NULL
             AND income_stream_id IS NULL AND category_id IS NULL AND account_id IS NULL
             AND amount_minor IS NULL AND rate_bps IS NOT NULL
             AND duration_months IS NOT NULL
             AND delay_months IS NULL AND ramp_months IS NULL
        WHEN 'recurring_delta' THEN category_id IS NOT NULL
             AND income_stream_id IS NULL AND person_id IS NULL AND account_id IS NULL
             AND amount_minor IS NOT NULL AND rate_bps IS NULL
             AND delay_months IS NOT NULL AND ramp_months IS NOT NULL
        WHEN 'set_baseline' THEN ((category_id IS NOT NULL) <> (account_id IS NOT NULL))
             AND income_stream_id IS NULL AND person_id IS NULL
             AND amount_minor IS NOT NULL AND rate_bps IS NULL
             AND delay_months IS NULL AND ramp_months IS NULL AND duration_months IS NULL
        WHEN 'one_off_amount' THEN ((category_id IS NOT NULL) <> (account_id IS NOT NULL))
             AND income_stream_id IS NULL AND person_id IS NULL
             AND amount_minor IS NOT NULL AND rate_bps IS NULL
             AND delay_months IS NULL AND ramp_months IS NULL AND duration_months IS NULL
        -- An account and a factor, and nothing else. No upper bound on the factor: a 100x round is
        -- unlikely, not impossible, and a ceiling here would be a view on venture outcomes the
        -- schema has no business holding. Zero is allowed and means the holding is worthless.
        WHEN 'revalue' THEN account_id IS NOT NULL
             AND income_stream_id IS NULL AND person_id IS NULL AND category_id IS NULL
             AND amount_minor IS NULL AND rate_bps IS NOT NULL AND rate_bps >= 0
             AND delay_months IS NULL AND ramp_months IS NULL AND duration_months IS NULL
        -- A share of the account, so bounded at 100%: selling 150% of a holding is not a scenario,
        -- it is a typo, and the projection would happily produce cash from it.
        WHEN 'liquidate' THEN account_id IS NOT NULL
             AND income_stream_id IS NULL AND person_id IS NULL AND category_id IS NULL
             AND amount_minor IS NULL
             AND rate_bps IS NOT NULL AND rate_bps > 0 AND rate_bps <= 10000
             AND delay_months IS NULL AND ramp_months IS NULL AND duration_months IS NULL
        ELSE 0
    END)
) STRICT;

INSERT INTO forecast_event_effects_new
    (id, event_id, kind, sort_order, income_stream_id, person_id, category_id, account_id,
     amount_minor, rate_bps, delay_months, ramp_months, duration_months, created_at)
SELECT id, event_id, kind, sort_order, income_stream_id, person_id, category_id, account_id,
       amount_minor, rate_bps, delay_months, ramp_months, duration_months, created_at
  FROM forecast_event_effects;

DROP TABLE forecast_event_effects;
ALTER TABLE forecast_event_effects_new RENAME TO forecast_event_effects;
CREATE INDEX idx_forecast_event_effects_event  ON forecast_event_effects(event_id);
CREATE INDEX idx_forecast_event_effects_stream ON forecast_event_effects(income_stream_id);
CREATE INDEX idx_forecast_event_effects_person ON forecast_event_effects(person_id);

CREATE TABLE forecast_event_relations_new (
    id                  INTEGER PRIMARY KEY,
    event_id            INTEGER NOT NULL REFERENCES forecast_events(id) ON DELETE CASCADE,
    depends_on_event_id INTEGER NOT NULL REFERENCES forecast_events(id),
    -- 'only_if_not' is the complement of 'only_if', and it is what makes two outcomes a partition
    -- rather than two coin flips. Deletion costs the same as 'only_if' and is refused the same way:
    -- an event the user believed was conditional silently becoming unconditional is the change of
    -- meaning that RESTRICT exists to prevent, and it is no less silent for the condition being a
    -- negative one.
    kind                TEXT NOT NULL CHECK (kind IN ('after', 'only_if', 'only_if_not')),
    min_gap_months      INTEGER NOT NULL DEFAULT 0 CHECK (min_gap_months >= 0),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK (event_id <> depends_on_event_id)
) STRICT;

INSERT INTO forecast_event_relations_new
    (id, event_id, depends_on_event_id, kind, min_gap_months, created_at)
SELECT id, event_id, depends_on_event_id, kind, min_gap_months, created_at
  FROM forecast_event_relations;

DROP TABLE forecast_event_relations;
ALTER TABLE forecast_event_relations_new RENAME TO forecast_event_relations;
CREATE UNIQUE INDEX idx_forecast_event_relations_edge
    ON forecast_event_relations(event_id, depends_on_event_id, kind);
CREATE INDEX idx_forecast_event_relations_depends
    ON forecast_event_relations(depends_on_event_id);
