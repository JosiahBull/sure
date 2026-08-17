-- Income payments: the first table that links a *configured* income to the money that actually
-- arrived. Until now the two views of a salary never met — `income_streams` models what should
-- be paid (forecast-only), and `transactions` records what landed, with nothing connecting a
-- payday to its deposit. This table is that connection, one row per expected payment per stream.
--
-- The design copies `cron_runs`' idempotence pattern: a unique key per (stream, period) makes
-- regenerating the expected schedule a no-op where rows already exist, and each match points at
-- the artifact it claimed so it can be undone one row at a time.
--
-- The stored decomposition (gross, PAYE, ACC, KiwiSaver, student loan) is *reconstructed from
-- the observed net* — the bank deposit is ground truth, and `sure_core::tax::reconstruct_period`
-- inverts the per-period PAYE arithmetic so the lines always reconcile exactly to what landed.
-- It is materialised here rather than recomputed per request because it is a fact about a
-- specific matched deposit under the tax scale in force on its date: recomputing it later,
-- under an edited scale or an edited stream, would silently restate history.
--
-- One transaction may satisfy SEVERAL rows at once — a quarterly bonus paid inside the regular
-- salary run is two streams landing in one deposit — so `transaction_id` is deliberately not
-- unique. Each row carries its own slice (`observed_net_minor`), and the slices of a shared
-- transaction sum to the deposit.
--
-- `ON DELETE SET NULL` on `transaction_id` (an undone import deletes transactions by provider
-- tag): the matcher treats a matched/confirmed row whose transaction is gone as broken and
-- resets it to `expected`, clearing the decomposition. No CHECK ties `status` to
-- `transaction_id`, deliberately — SET NULL updates the child row, and a cross-column CHECK
-- would turn the parent delete into a constraint failure.

CREATE TABLE income_payments (
    id                       INTEGER PRIMARY KEY,
    income_stream_id         INTEGER NOT NULL REFERENCES income_streams(id) ON DELETE CASCADE,
    -- The scheduled date (YYYY-MM-DD), enumerated from the stream's anchor and frequency. The
    -- deposit may land up to a few days earlier (payroll shifts off weekends and holidays);
    -- this stays the *scheduled* date so the unique key is stable across re-runs.
    due_on                   TEXT NOT NULL,
    -- expected: enumerated, no deposit claimed yet (past-due expected rows are the "missed pay"
    --           the review UI surfaces). matched: the matcher claimed a deposit automatically.
    -- confirmed: a person agreed with a match. dismissed: a person said this expected payment
    --           is not real (a contract gap, unpaid leave) — kept rather than deleted so the
    --           matcher does not resurrect it on the next run.
    status                   TEXT NOT NULL DEFAULT 'expected'
                                 CHECK (status IN ('expected','matched','confirmed','dismissed')),
    transaction_id           INTEGER REFERENCES transactions(id) ON DELETE SET NULL,
    matched_by               TEXT CHECK (matched_by IS NULL OR matched_by IN ('auto','manual')),
    -- What the stream's configured level predicted for this date, at match/generation time.
    -- Kept beside the observed figure because their gap is the reconciliation signal (a pay
    -- rise, a wrong KiwiSaver rate) and recomputing it later under an edited stream would
    -- erase the evidence.
    expected_net_minor       INTEGER,
    -- This stream's slice of the claimed deposit. Equal to the transaction amount for a lone
    -- salary; smaller when a bonus shares the deposit.
    observed_net_minor       INTEGER,
    -- The reconstructed decomposition of that slice. NULL until matched, and cleared if the
    -- match is undone. gross − income_tax − acc − kiwisaver − student_loan == observed_net.
    gross_minor              INTEGER,
    income_tax_minor         INTEGER,
    acc_levy_minor           INTEGER,
    kiwisaver_minor          INTEGER,
    student_loan_minor       INTEGER,
    -- Computed alongside, never part of take-home (see sure_core::tax::PayeBreakdown).
    employer_kiwisaver_minor INTEGER,
    esct_minor               INTEGER,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (income_stream_id, due_on)
) STRICT;
CREATE INDEX idx_income_payments_tx ON income_payments(transaction_id);
CREATE INDEX idx_income_payments_status ON income_payments(status, due_on);

-- How a stream finds its deposits. Matching is on iff both are set: an account to look in and a
-- case-insensitive substring its description must carry (the bank's payroll memo — stable per
-- employer, unlike the run numbers appended after it). Substring rather than regex: every payroll
-- memo seen so far is distinguished by one token, and a regex column invites patterns whose
-- failure mode is silence.
ALTER TABLE income_streams ADD COLUMN match_account_id INTEGER REFERENCES accounts(id) ON DELETE SET NULL;
ALTER TABLE income_streams ADD COLUMN match_pattern TEXT;
-- How a payment is taxed when it arrives: as an ordinary payslip, or as an IRD "extra pay" (a
-- lump sum inside a regular pay run — a bonus, a back payment). Extra pays take a flat-rate slice
-- across the brackets, student loan with no threshold, and share the regular salary's deposit;
-- see sure_core::tax::extra_pay. A per-stream enum rather than a flag on the payment because it
-- is a fact about the *income*, not about one arrival of it.
ALTER TABLE income_streams ADD COLUMN pay_treatment TEXT NOT NULL DEFAULT 'regular'
    CHECK (pay_treatment IN ('regular','extra_pay'));
