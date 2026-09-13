-- Income a household earns jointly, rather than one of its people earning it.
--
-- 0021 made every stream one person's: "WHOSE. `person_id` … A career break belongs to
-- someone; a household average has nobody to take it." That is right about a salary and wrong
-- about a house. Rent from a flatmate is paid to the household, and halving it so the schema
-- can hold it would invent a division nobody made and then put a per-person figure on the
-- forecast that is not true of either person.
--
-- So a stream gets the same `(ownership, person_id)` pair an account has carried since 0015,
-- parsed into the same `sure_core::Ownership` the moment a row is read. Two states, matching
-- accounts after 0016 removed the third:
--
--   'person' + person_id -> that individual earns it (every row before this migration)
--   'joint'  + NULL      -> the household earns it (rent from a flatmate, a refund it is owed)
--
-- **A joint stream must be `basis = 'net'`, and that is a restriction rather than an
-- oversight.** A gross figure only means something beside the person whose marginal rate
-- applies to it: `sure_app::income::take_home` prices every gross stream against
-- `person_annual_gross`, the sum of *that person's* streams, because PAYE brackets are
-- progressive over total income. A joint gross stream has no such person, and both ways out
-- are worse than refusing it — pricing it against the household's combined income taxes it at
-- a rate neither person pays, and halving it assumes a 50/50 beneficial interest nothing here
-- records. Boarder income under IRD's standard-cost rules is untaxed and lands here naturally;
-- a jointly owned *rental*, which is taxed to each owner on their share, wants two streams and
-- a real share split, and should not be quietly mispriced while waiting for one.
--
-- Said in three places on purpose: the CHECK below, which cannot be talked out of it;
-- `SaveIncomeStream`'s validation, so the API answers 422 with a reason instead of a constraint
-- violation; and the type, since `Ownership::Joint` carries no person to look a scale up
-- against.
--
-- `person_id` has to become nullable, which SQLite cannot do in place, so the table is rebuilt.
-- Every column below was read off the live schema rather than off 0021 — five later migrations
-- had added to it (`employer_kiwisaver_bps`, the two contribution-target accounts, the matcher's
-- pair, `pay_treatment`) and `semi_monthly` had been added to the `pay_frequency` CHECK, all of
-- which a rebuild transcribed from the original file would have silently dropped on the floor.
-- `legacy_alter_table` is off in the SQLite sqlx ships, so `income_stream_steps` and
-- `income_payments` follow the rename rather than pointing at the dropped table.
--
-- **The children are copied out and back, and that is not belt-and-braces.** Both declare
-- `REFERENCES income_streams(id) ON DELETE CASCADE`, and sqlx connects with
-- `PRAGMA foreign_keys = ON`, so `DROP TABLE income_streams` below *deletes every row in both of
-- them*. `PRAGMA foreign_keys` cannot be changed inside a transaction and a migration runs in
-- one, so turning it off for the rebuild is not available. Holding tables are.
--
-- This was not hypothetical: the first version of this file shipped without them and destroyed
-- nine `income_stream_steps` rows — a pay-scale schedule — on the first database it touched.
-- `income_payments` came back only because `sure_app::tasks::income_match` rebuilds it from the
-- transactions, which is exactly the kind of luck that hides this. A rebuild verified against an
-- empty database proves the schema and nothing about the data.

CREATE TABLE income_stream_steps_backup AS SELECT * FROM income_stream_steps;
CREATE TABLE income_payments_backup AS SELECT * FROM income_payments;

CREATE TABLE income_streams_new (
    id                   INTEGER PRIMARY KEY,
    ownership            TEXT NOT NULL DEFAULT 'person',
    person_id            INTEGER REFERENCES people(id) ON DELETE RESTRICT,
    label                TEXT NOT NULL,
    employer             TEXT,
    currency_code        TEXT NOT NULL REFERENCES currencies(code),
    annual_amount_minor  INTEGER NOT NULL CHECK (annual_amount_minor > 0),
    basis                TEXT NOT NULL CHECK (basis IN ('net', 'gross_nz_paye')),
    pay_frequency        TEXT NOT NULL CHECK (pay_frequency IN
                             ('weekly','fortnightly','four_weekly','semi_monthly',
                              'monthly','quarterly','annual')),
    first_payment_on     TEXT NOT NULL,
    starts_on            TEXT NOT NULL,
    ends_on              TEXT CHECK (ends_on IS NULL OR ends_on > starts_on),
    annual_increase_bps  INTEGER NOT NULL DEFAULT 0,
    kiwisaver_bps        INTEGER NOT NULL DEFAULT 0
                             CHECK (kiwisaver_bps BETWEEN 0 AND 10000),
    student_loan         INTEGER NOT NULL DEFAULT 0 CHECK (student_loan IN (0,1)),
    take_home_bps        INTEGER CHECK (take_home_bps IS NULL OR take_home_bps BETWEEN 0 AND 10000),
    linked_category_id   INTEGER REFERENCES categories(id) ON DELETE RESTRICT,
    enabled              INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    sort_order           INTEGER NOT NULL DEFAULT 0,
    notes                TEXT,
    employer_kiwisaver_bps INTEGER NOT NULL DEFAULT 0
                             CHECK (employer_kiwisaver_bps BETWEEN 0 AND 10000),
    kiwisaver_account_id INTEGER REFERENCES accounts(id) ON DELETE RESTRICT,
    student_loan_account_id INTEGER REFERENCES accounts(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    match_account_id     INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
    match_pattern        TEXT,
    pay_treatment        TEXT NOT NULL DEFAULT 'regular'
                             CHECK (pay_treatment IN ('regular','extra_pay')),
    -- The 0015 invariant, as a real table CHECK: a rebuild is the one moment SQLite allows one,
    -- so the pair of triggers `accounts` needed is not required here.
    CHECK (ownership IN ('person', 'joint')),
    CHECK ((ownership = 'person') = (person_id IS NOT NULL)),
    -- Why a joint stream cannot be gross: see the header.
    CHECK (ownership = 'person' OR basis = 'net')
) STRICT;

-- Every existing row is one person's and stays that way, so this migration moves no figure in
-- any projection.
INSERT INTO income_streams_new (
    id, ownership, person_id, label, employer, currency_code, annual_amount_minor, basis,
    pay_frequency, first_payment_on, starts_on, ends_on, annual_increase_bps, kiwisaver_bps,
    student_loan, take_home_bps, linked_category_id, enabled, sort_order, notes,
    employer_kiwisaver_bps, kiwisaver_account_id, student_loan_account_id,
    created_at, updated_at, match_account_id, match_pattern, pay_treatment
)
SELECT
    id, 'person', person_id, label, employer, currency_code, annual_amount_minor, basis,
    pay_frequency, first_payment_on, starts_on, ends_on, annual_increase_bps, kiwisaver_bps,
    student_loan, take_home_bps, linked_category_id, enabled, sort_order, notes,
    employer_kiwisaver_bps, kiwisaver_account_id, student_loan_account_id,
    created_at, updated_at, match_account_id, match_pattern, pay_treatment
FROM income_streams;

DROP TABLE income_streams;
ALTER TABLE income_streams_new RENAME TO income_streams;

-- Whatever the cascade did or did not take, both children end up holding exactly what they held
-- before: emptied, refilled from the copies, and the copies dropped.
DELETE FROM income_stream_steps;
INSERT INTO income_stream_steps SELECT * FROM income_stream_steps_backup;
DROP TABLE income_stream_steps_backup;

DELETE FROM income_payments;
INSERT INTO income_payments SELECT * FROM income_payments_backup;
DROP TABLE income_payments_backup;

CREATE INDEX idx_income_streams_person ON income_streams(person_id);
CREATE INDEX idx_income_streams_category ON income_streams(linked_category_id);
CREATE INDEX idx_income_streams_kiwisaver_account ON income_streams(kiwisaver_account_id);
CREATE INDEX idx_income_streams_student_loan_account ON income_streams(student_loan_account_id);
