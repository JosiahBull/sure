-- Probabilistic life events: the first thing in the forecast the simulation is allowed to disagree
-- with itself about.
--
-- `forecast_events` (0013) modelled a certainty — "my bonus lands in March" — applied identically to
-- every path. A child, a promotion, a career break are not that shape. The household knows roughly
-- when and roughly how likely, and the useful answer is not one line but the spread: in what
-- fraction of futures can we afford this, and how much does the timing matter.
--
-- So rather than a second table beside the first, `forecast_events` is rebuilt as the superset. The
-- old rows migrate in as certainties (100% likely, no spread, one effect), which is exactly what
-- they were, and the two concepts stop being two concepts. A separate `life_events` table would
-- have left two paths differing by a hyphenated word, both returning things called "events", for
-- every future reader of the schema and the OpenAPI document to squint at.
--
-- WHY THE EFFECTS ARE COLUMNS AND NOT A JSON TAGGED UNION.
-- `AccountMetadata` is this codebase's model for a tagged union stored as JSON, and it is right for
-- what it holds: lenders, addresses, rates, model years. *Values.* Not one of its fields is a
-- foreign key. An effect is almost entirely foreign keys — a promotion names an income stream, a
-- career break names a person, daycare names a category, a lump sum names an account. In JSON,
-- `DELETE FROM income_streams WHERE id = 7` succeeds and the promotion silently starts pointing at
-- nothing; and the 409 `people::delete` already writes ("Re-attribute or delete the accounts owned
-- by this person first: ...") becomes impossible to write honestly, because there is no column to
-- `SELECT ... WHERE income_stream_id = ?1`. That would be referential integrity reimplemented in
-- application code over an unindexable json_extract.
--
-- The line, stated once: JSON tagged union for value payloads; typed columns for references. The
-- union still exists — in Rust, with an `as_columns`/`from_columns` pair, the same seam
-- `Ownership::as_parts`/`from_stored` is, and with the same contract: a combination that should be
-- impossible is an error naming the row, never a value coerced into whichever variant looks closest.

-- Renamed out of the way first, so the child tables below can be created against the final name and
-- so dropping it at the end takes its indexes with it rather than colliding with the new ones.
ALTER TABLE forecast_events RENAME TO forecast_events_legacy;

CREATE TABLE forecast_events (
    id                   INTEGER PRIMARY KEY,
    label                TEXT NOT NULL,
    -- Presentation and form template ONLY. The simulation branches on an event's *effects*, never
    -- on its kind, so adding 'sabbatical' next year is a UI change and not a simulation change.
    kind                 TEXT NOT NULL CHECK (kind IN
                             ('promotion','child','career_break','job_start','job_end',
                              'adjustment','custom')),
    -- Whose event it is. NULL = the household (a child, a house move). RESTRICT: a person's career
    -- break silently becoming nobody's is worse than a 409 that names it.
    person_id            INTEGER REFERENCES people(id) ON DELETE RESTRICT,
    expected_on          TEXT NOT NULL,
    -- Half-width of a UNIFORM HARD window, in months. Every month inside it is equally likely and
    -- nothing lands outside — so "±2 years" means what it says rather than being a distribution's
    -- 90th percentile. 0 = the date is certain and only the occurrence is not.
    timing_spread_months INTEGER NOT NULL DEFAULT 0
                             CHECK (timing_spread_months BETWEEN 0 AND 600),
    -- 0 = modelled but off (still reported, so the UI shows a disabled row rather than losing it);
    -- 10000 = certain to happen, with possibly-uncertain timing.
    probability_bps      INTEGER NOT NULL DEFAULT 10000
                             CHECK (probability_bps BETWEEN 0 AND 10000),
    notes                TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE forecast_event_effects (
    id                INTEGER PRIMARY KEY,
    -- CASCADE: an effect has no meaning without its event. Composition, not reference — contrast
    -- `depends_on_event_id` below, which is refused instead.
    event_id          INTEGER NOT NULL REFERENCES forecast_events(id) ON DELETE CASCADE,
    kind              TEXT NOT NULL CHECK (kind IN (
                          'income_step',      -- a promotion: step a stream's level
                          'income_start',     -- a job begins
                          'income_end',       -- a job ends
                          'income_pause',     -- a career break: pause someone's pay
                          'recurring_delta',  -- a child: daycare, delayed, ramped, maybe ending
                          'set_baseline',     -- the legacy step_change
                          'one_off_amount')), -- the legacy one_off_amount
    sort_order        INTEGER NOT NULL DEFAULT 0,
    -- RESTRICT on all four: deleting any of them while an effect points at it is a 409 naming the
    -- events, the way deleting a person who owns accounts already is.
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

    -- The shape of each kind, as data. `(a IS NOT NULL) <> (b IS NOT NULL)` is SQLite's XOR, the
    -- idiom 0016's ownership triggers already use.
    --
    -- The `ELSE 0` is load-bearing: with no ELSE, a kind added to the enum above but forgotten here
    -- returns NULL, and a CHECK *passes* on NULL — so the new kind would silently accept any shape.
    CHECK (CASE kind
        WHEN 'income_step' THEN income_stream_id IS NOT NULL
             AND person_id IS NULL AND category_id IS NULL AND account_id IS NULL
             -- A new absolute figure, or a percentage uplift. Exactly one: a promotion is one or
             -- the other, and both would be two answers to the same question.
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
             -- Both required. A break of unstated length is not a break, and unstated replacement
             -- pay is the difference between parental leave and resigning.
             AND duration_months IS NOT NULL AND duration_months > 0
             AND rate_bps IS NOT NULL AND rate_bps BETWEEN 0 AND 10000
             AND amount_minor IS NULL AND delay_months IS NULL AND ramp_months IS NULL
        WHEN 'recurring_delta' THEN category_id IS NOT NULL
             AND income_stream_id IS NULL AND person_id IS NULL AND account_id IS NULL
             -- Base reporting currency, matching what a category-targeted amount already means.
             AND amount_minor IS NOT NULL
             -- Daycare does not start the day the child is born, and school fees do not stop.
             AND delay_months IS NOT NULL AND delay_months >= 0
             AND ramp_months  IS NOT NULL AND ramp_months  >= 0
             AND (duration_months IS NULL OR duration_months > 0)
             AND rate_bps IS NULL
        WHEN 'set_baseline' THEN ((account_id IS NOT NULL) <> (category_id IS NOT NULL))
             AND income_stream_id IS NULL AND person_id IS NULL
             AND amount_minor IS NOT NULL AND rate_bps IS NULL
             AND delay_months IS NULL AND ramp_months IS NULL AND duration_months IS NULL
        WHEN 'one_off_amount' THEN ((account_id IS NOT NULL) <> (category_id IS NOT NULL))
             AND income_stream_id IS NULL AND person_id IS NULL
             AND amount_minor IS NOT NULL AND rate_bps IS NULL
             AND delay_months IS NULL AND ramp_months IS NULL AND duration_months IS NULL
        ELSE 0
    END)
) STRICT;

-- Ordering and conditionality between events: "the promotion has to happen before children".
--
-- Two kinds, and only two.
--   'after'   — this event's sampled month is at least `min_gap_months` after its parent's, applied
--               by CLAMPING the child's month up, never by resampling. An unbounded
--               reject-and-retry makes the number of RNG values consumed depend on the draws, and a
--               seeded forecast stops being reproducible — the argument `AmortSchedule::open`
--               already makes for clamping a refix rate.
--               Deliberately one-directional: one direction means the edge set *is* the dependency
--               graph. A mirrored 'before' would let someone draw a cycle out of two constraints
--               that each read as sensible. The UI still offers "before" and writes the reversed
--               edge, which is a presentation concern and belongs there.
--   'only_if' — this event can occur only on a path where its parent occurred.
-- An 'after' whose parent did not occur is vacuous, not blocking. If conditionality was meant, that
-- is the other kind, and both can be set on the same pair.
CREATE TABLE forecast_event_relations (
    id                  INTEGER PRIMARY KEY,
    -- CASCADE: deleting an event takes its own OUTGOING constraints with it.
    event_id            INTEGER NOT NULL REFERENCES forecast_events(id) ON DELETE CASCADE,
    -- No FK action, because deletion is resolved in the DAL and the two kinds differ in what
    -- deletion *costs*. A dangling 'after' is pure ordering and is dropped silently. A dangling
    -- 'only_if' is not: an event the user believed was conditional would quietly become
    -- unconditional in every future projection — a change of meaning with no trace, which is what
    -- `people`'s RESTRICT exists to prevent. So that case is a 409 naming the dependents.
    depends_on_event_id INTEGER NOT NULL REFERENCES forecast_events(id),
    kind                TEXT NOT NULL CHECK (kind IN ('after', 'only_if')),
    min_gap_months      INTEGER NOT NULL DEFAULT 0 CHECK (min_gap_months >= 0),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    -- The 1-cycle, the only one a constraint can catch. Longer ones need a graph walk, and that
    -- runs twice: at write time (a 409 naming the cycle) and at resolve time — the same two layers
    -- `categories` uses, where `validate` rejects a parent cycle on write and `Categories::chain`
    -- still carries a seen-check so a hand-edited database cannot hang a report.
    CHECK (event_id <> depends_on_event_id)
) STRICT;

-- Carry the old rows across as what they always were: certainties with a single effect. `id` is
-- preserved so the effect can be inserted against it, and so any external reference still resolves.
INSERT INTO forecast_events (id, label, kind, expected_on, timing_spread_months,
                             probability_bps, created_at)
SELECT id, label, 'adjustment', effective_date, 0, 10000, created_at
  FROM forecast_events_legacy;

INSERT INTO forecast_event_effects (event_id, kind, account_id, category_id, amount_minor)
SELECT id,
       CASE kind WHEN 'step_change' THEN 'set_baseline' ELSE 'one_off_amount' END,
       CASE target_type WHEN 'account'  THEN target_id END,
       CASE target_type WHEN 'category' THEN target_id END,
       amount_minor
  FROM forecast_events_legacy;

DROP TABLE forecast_events_legacy;

CREATE INDEX idx_forecast_events_person ON forecast_events(person_id);
CREATE INDEX idx_forecast_event_effects_event  ON forecast_event_effects(event_id);
-- The three lookups the 409s are written from.
CREATE INDEX idx_forecast_event_effects_stream ON forecast_event_effects(income_stream_id);
CREATE INDEX idx_forecast_event_effects_person ON forecast_event_effects(person_id);
CREATE UNIQUE INDEX idx_forecast_event_relations_edge
    ON forecast_event_relations(event_id, depends_on_event_id, kind);
CREATE INDEX idx_forecast_event_relations_depends
    ON forecast_event_relations(depends_on_event_id);
