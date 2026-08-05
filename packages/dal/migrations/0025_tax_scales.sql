-- Tax rules the user can edit, rather than ones only a release can change.
--
-- `sure_core::tax` holds New Zealand's brackets, levies and thresholds as dated constants, which is
-- right for a *default* and wrong for the only copy: IRD changes a threshold and the projection is
-- quietly wrong until someone ships a binary. Anyone modelling thirty years out is also entitled to
-- ask "what if the top rate goes up", which a constant cannot answer at all.
--
-- So the constants become a **seed and a fallback**, and this table is authoritative once it has
-- rows. `sure_dal::tax_scales::seed` copies them in after migration when the table is empty, so
-- there is exactly one place the figures are written down and no chance of two copies drifting —
-- which is why the seeding is Rust rather than INSERT statements in this file.
--
-- The brackets are JSON, and that is deliberate rather than lazy. They are pure *values* — an
-- ordered list of (bound, rate) pairs — with no foreign keys and nothing to enforce referential
-- integrity against, which is exactly the line 0022 drew: JSON for value payloads, typed columns
-- for references. A `tax_brackets` child table would add two more queries and a sort order to get
-- wrong, in exchange for constraints on data that is validated on the way in anyway.
CREATE TABLE tax_scales (
    id                           INTEGER PRIMARY KEY,
    -- Which jurisdiction's rules these are. Matches `sure_core::tax::TaxScaleId`.
    scale_id                     TEXT NOT NULL CHECK (scale_id IN ('nz_paye')),
    -- Scales are dated and the latest one not after a given date wins, so editing history is
    -- possible but never accidental: adding a row is how you change next year.
    effective_from               TEXT NOT NULL,
    -- JSON `[[upper_bound_annual_minor, rate_bps], ...]`, ascending, last bound `null` for "and
    -- above". `null` rather than a huge sentinel because JSON has no i64::MAX and a literal
    -- 9223372036854775807 in a settings screen is nobody's idea of a threshold.
    brackets                     TEXT NOT NULL,
    acc_levy_bps                 INTEGER NOT NULL CHECK (acc_levy_bps BETWEEN 0 AND 10000),
    acc_income_cap_minor         INTEGER NOT NULL CHECK (acc_income_cap_minor >= 0),
    student_loan_threshold_minor INTEGER NOT NULL CHECK (student_loan_threshold_minor >= 0),
    student_loan_rate_bps        INTEGER NOT NULL CHECK (student_loan_rate_bps BETWEEN 0 AND 10000),
    -- Same shape as `brackets`. A flat rate chosen by bracket, not a progressive slice.
    esct_brackets                TEXT NOT NULL,
    -- The government's KiwiSaver contribution: what it adds per dollar the *member* puts in, the
    -- annual ceiling, and the income above which nothing is paid. NULL on the cap means no income
    -- test, which is how the years before Budget 2025 worked.
    kiwisaver_govt_match_bps     INTEGER NOT NULL DEFAULT 0
                                     CHECK (kiwisaver_govt_match_bps BETWEEN 0 AND 10000),
    kiwisaver_govt_max_minor     INTEGER NOT NULL DEFAULT 0
                                     CHECK (kiwisaver_govt_max_minor >= 0),
    kiwisaver_govt_income_cap_minor INTEGER
                                     CHECK (kiwisaver_govt_income_cap_minor IS NULL
                                            OR kiwisaver_govt_income_cap_minor >= 0),
    -- Where these figures came from, so a future reader can check them rather than trust them. The
    -- seeded rows carry the sources they were read off.
    source_note                  TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- One scale per jurisdiction per start date: two rows claiming the same day is a typo, and which
-- one won would depend on insertion order.
CREATE UNIQUE INDEX idx_tax_scales_effective ON tax_scales(scale_id, effective_from);
