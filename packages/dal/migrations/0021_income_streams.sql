-- Per-person income streams: the first thing in this schema that models *earning* rather than
-- observing money already earned.
--
-- Until now a forecast's income was a fitted top-level income category and nothing else.
-- `sure_app::forecast::resolve_category_assumptions` says so in a comment: "splitting it per
-- person would need per-person income/expense assumptions that don't exist." A category baseline
-- is an average of what landed in the bank, and it structurally cannot answer four questions:
--
--   * WHOSE.  `person_id`, the same people rows accounts and transactions already point at. A
--             career break belongs to someone; a household average has nobody to take it.
--   * WHEN.   A pay frequency plus an anchor date. The simulation steps in months, so a quarterly
--             or annual payment has to land in the month it really lands in, and a fortnightly
--             one has to produce three paydays in the months that really have three. Dates are
--             enumerated from `first_payment_on` rather than divided by twelve — the same
--             reasoning `monthly_repayment` gives for x52/12 over x4, except here the extra
--             payday is visible in the month it falls in.
--   * GROSS.  A salary is quoted before tax; the fitted baselines are net money in the bank.
--             `basis` says which one the figure is, and `sure_core::tax` bridges them.
--   * STEPS.  A teacher's pay scale increments on known dates years ahead. Those are certainties,
--             so they live in their own dated table below and apply identically on every
--             simulated path, exactly as a `forecast_events` step change does.
--
-- `linked_category_id` is the seam that stops double counting, and it is the load-bearing column
-- in this file. A stream already landing in the bank is *also* inside the fitted baseline of the
-- income category it lands in; without the link the forecast would count every salary twice. With
-- it, the category's baseline is reduced by the streams' own modelled net and only the residual --
-- the income the streams do not explain, like interest or a gift -- is projected from history.
-- Netting rather than excluding the category outright is deliberate: excluding it would silently
-- drop that residual. The coverage is reported, so "your streams explain 96% of Salary" is a
-- number on the response rather than an assumption nobody can check.

CREATE TABLE income_streams (
    id                   INTEGER PRIMARY KEY,
    -- RESTRICT, matching accounts.person_id (0015): deleting someone who still earns would
    -- silently delete their income from every projection. The DAL refuses with a 409 naming the
    -- streams long before this fires.
    person_id            INTEGER NOT NULL REFERENCES people(id) ON DELETE RESTRICT,
    label                TEXT NOT NULL,
    -- Free text: an employer name is not a closed set (see CLAUDE.md rule 2's list).
    employer             TEXT,
    currency_code        TEXT NOT NULL REFERENCES currencies(code),
    -- The quoted annual figure, in whichever direction `basis` says. Per-payment is derived
    -- (`annual / periods_per_year`) because that is how payroll works: a fortnightly salary is
    -- quoted annually and divided by 26, and the years containing 27 paydays really do pay 27
    -- times.
    annual_amount_minor  INTEGER NOT NULL CHECK (annual_amount_minor > 0),
    -- Whether the figure above is take-home or before deductions -- and, when it is before, which
    -- jurisdiction's rules apply. ONE column rather than a separate `taxable` flag beside a
    -- `tax_scale`: those would be two independent encodings of the same fact, free to drift, which
    -- is precisely what CLAUDE.md rule 1 exists to prevent. Adding another jurisdiction is a new
    -- value here and a new arm in `IncomeBasis`, which the exhaustive-match lint then finds for
    -- you at every site that has to decide what it means.
    basis                TEXT NOT NULL CHECK (basis IN ('net', 'gross_nz_paye')),
    pay_frequency        TEXT NOT NULL CHECK (pay_frequency IN
                             ('weekly','fortnightly','four_weekly','monthly','quarterly','annual')),
    -- The anchor. Every payment date is this plus k periods; a monthly/quarterly/annual stream
    -- clamps its day-of-month to the target month's length, as `add_months` and `crons::period_date`
    -- already do.
    first_payment_on     TEXT NOT NULL,
    starts_on            TEXT NOT NULL,
    ends_on              TEXT CHECK (ends_on IS NULL OR ends_on > starts_on),
    -- The residual annual increase applied after the last dated step. 0 is the honest default: an
    -- unstated pay rise is a guess, and over a thirty-year horizon it compounds.
    annual_increase_bps  INTEGER NOT NULL DEFAULT 0,
    -- Employee KiwiSaver rate and whether IR deducts student loan. Only consulted when `basis` is
    -- a gross one; kept unconditionally because a stream that switches from net to gross should
    -- not lose them.
    kiwisaver_bps        INTEGER NOT NULL DEFAULT 0
                             CHECK (kiwisaver_bps BETWEEN 0 AND 10000),
    student_loan         INTEGER NOT NULL DEFAULT 0 CHECK (student_loan IN (0,1)),
    -- An explicit gross->net override, in basis points. NULL = resolve it (the statutory scale,
    -- then reconciliation against this household's own history) -- the same "NULL means derive
    -- this" contract every forecast_assumptions knob has.
    take_home_bps        INTEGER CHECK (take_home_bps IS NULL OR take_home_bps BETWEEN 0 AND 10000),
    -- The income category this stream's net pay lands in. RESTRICT for the same reason as
    -- person_id: deleting the category would un-net the stream and double-count it against a
    -- baseline that no longer exists. NULL is legal for a stream that has not started yet (no
    -- history to overlap), which the DAL enforces -- a CHECK cannot compare a column to today.
    linked_category_id   INTEGER REFERENCES categories(id) ON DELETE RESTRICT,
    enabled              INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    sort_order           INTEGER NOT NULL DEFAULT 0,
    notes                TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX idx_income_streams_person ON income_streams(person_id);
CREATE INDEX idx_income_streams_category ON income_streams(linked_category_id);

-- Dated pay-scale steps.
--
-- A teacher's scale increments on published dates for years ahead; a graduate programme has a
-- stated second-year figure. These are things the household knows, not things to put a
-- distribution on -- so they apply identically on every simulated path, the same contract
-- `forecast_events` has ("a certainty the user is asserting, not a statistical estimate").
--
-- Separate rows rather than a JSON array because a schedule is a list of dated facts the UI edits
-- one at a time. And the step is the *new absolute figure*, not a delta: scales are published as
-- absolutes, and storing deltas would make an edit to step 2 silently move steps 3..n.
CREATE TABLE income_stream_steps (
    id                  INTEGER PRIMARY KEY,
    income_stream_id    INTEGER NOT NULL REFERENCES income_streams(id) ON DELETE CASCADE,
    effective_on        TEXT NOT NULL,
    annual_amount_minor INTEGER NOT NULL CHECK (annual_amount_minor > 0),
    label               TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
-- Two different salaries starting the same day is a typo, not a schedule.
CREATE UNIQUE INDEX idx_income_stream_steps_stream_date
    ON income_stream_steps(income_stream_id, effective_on);
