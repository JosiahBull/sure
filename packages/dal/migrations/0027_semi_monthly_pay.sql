-- Twice a month is a pay cadence, and the CHECK constraint did not know about it.
--
-- `income_streams.pay_frequency` was written when `PayFrequency` had six variants. Adding
-- `semi_monthly` in Rust left the column refusing it, so recording a salary paid on the 14th and
-- 28th failed at the database rather than at the form — and a CHECK violation is not a shape the
-- error mapping recognised, so it surfaced as a 500.
--
-- It matters because those two cadences are constantly conflated: "fortnightly, on the 14th and
-- 28th" describes twice a month, which is 24 payments a year where every fourteen days is 26. On a
-- $135,000 salary that is $5,625 a payslip against $5,192.
--
-- SQLite cannot alter a CHECK, so this is the table rebuild: create beside, copy, drop, rename.
-- Indexes are recreated afterwards because dropping the old table takes its own with it.
CREATE TABLE income_streams_new (
    id                   INTEGER PRIMARY KEY,
    person_id            INTEGER NOT NULL REFERENCES people(id) ON DELETE RESTRICT,
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
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

INSERT INTO income_streams_new
SELECT id, person_id, label, employer, currency_code, annual_amount_minor, basis, pay_frequency,
       first_payment_on, starts_on, ends_on, annual_increase_bps, kiwisaver_bps, student_loan,
       take_home_bps, linked_category_id, enabled, sort_order, notes, employer_kiwisaver_bps,
       kiwisaver_account_id, student_loan_account_id, created_at, updated_at
  FROM income_streams;

DROP TABLE income_streams;
ALTER TABLE income_streams_new RENAME TO income_streams;

CREATE INDEX idx_income_streams_person ON income_streams(person_id);
CREATE INDEX idx_income_streams_category ON income_streams(linked_category_id);
CREATE INDEX idx_income_streams_kiwisaver_account ON income_streams(kiwisaver_account_id);
CREATE INDEX idx_income_streams_student_loan_account ON income_streams(student_loan_account_id);
