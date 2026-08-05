-- Where a salary's deductions actually go.
--
-- KiwiSaver and student-loan deductions were computed correctly and then vanished: the take-home
-- figure was right, but the money came off the projection entirely. Over a thirty-year horizon that
-- is most of a retirement balance missing, and a student loan that never gets paid off.
--
-- THE TRAP, and the reason this is its own migration rather than a field on the last one.
--
-- Both target accounts are already in the projection, driven by a rate fitted from their own
-- history — and that history *is* the contributions. A KiwiSaver balance rising 15%/yr might be 8%
-- market and 7% contributions; a student loan falling $8k a year is falling because of the very
-- deductions this column would now add. Crediting the money on top of a rate that already contains
-- it counts it twice, and the error compounds for the whole horizon.
--
-- There is no way to separate the two components from a balance series, so the honest response is
-- not to try. When an account is a contribution target, `sure_app::forecast` stops using its fitted
-- rate: growth comes from an explicit override, else the long-run anchor, else flat, and the
-- assumption is reported as `contribution_driven` with a warning naming what was discarded. The
-- measured *volatility* is kept — month-to-month scatter is real either way.
--
-- That is also why these are opt-in columns rather than an inferred link. Turning them on changes
-- what the projection means for those accounts, so it has to be a choice someone made.

-- The account contributions land in. RESTRICT rather than SET NULL: silently unlinking would put the
-- account back on a fitted rate that has contributions baked into it, which is the double count this
-- whole file exists to avoid — and it would do it invisibly.
ALTER TABLE income_streams ADD COLUMN kiwisaver_account_id INTEGER
    REFERENCES accounts(id) ON DELETE RESTRICT;

-- The employer's contribution, in basis points of gross. Not a deduction from take-home — it is
-- money the employer adds on top — but it lands in the same account and over decades it is most of
-- the balance. 350 (3.5%) is the compulsory minimum from 1 April 2026, but it is stored rather than
-- assumed because plenty of employers pay more.
ALTER TABLE income_streams ADD COLUMN employer_kiwisaver_bps INTEGER NOT NULL DEFAULT 0
    CHECK (employer_kiwisaver_bps BETWEEN 0 AND 10000);

-- The student loan the `student_loan` deductions pay down. Same RESTRICT, same reason.
ALTER TABLE income_streams ADD COLUMN student_loan_account_id INTEGER
    REFERENCES accounts(id) ON DELETE RESTRICT;

CREATE INDEX idx_income_streams_kiwisaver_account ON income_streams(kiwisaver_account_id);
CREATE INDEX idx_income_streams_student_loan_account ON income_streams(student_loan_account_id);
