-- A KiwiSaver election is dated, exactly like the salary it comes out of.
--
-- `income_streams.kiwisaver_bps` is one number for all time. That is fine for projecting forward
-- and wrong the moment the matcher reconstructs a deposit from three years ago, because the rate
-- moves: an employee re-elects, and on 1 April 2026 the statutory default went 3% -> 3.5% and
-- took with it everyone who had never elected at all. The compulsory *employer* minimum stepped
-- on the same day, and steps again in 2028.
--
-- Reconstructing an old pay at today's rate does not fail loudly. `reconstruct_period` finds the
-- gross whose modelled net reaches the observed deposit and pushes the residual into income tax,
-- so the deposit still reconciles to the cent — it is the *split* that is wrong: the KiwiSaver
-- ribbon is too fat by the rate difference and PAYE quietly absorbs it. At 3.5% against a true
-- 3% that is half a percent of gross, every pay, silently.
--
-- Real history pins the mechanism to the cent, and the arithmetic is worth knowing because it is
-- how you recognise the problem in your own data: across 1 April 2026 a semi-monthly net drops by
-- 0.5% of gross per pay (the contribution step) plus 0.08% (the ACC earner levy's 1.67% -> 1.75%
-- step) on a salary that did not move at all. Read the other way, it is a diagnostic — invert a
-- run of pays back to an annual gross and the wrong contribution rate shows up as every figure
-- sitting a consistent fraction of a percent above a round salary, where the right one lands on
-- it. A salary is a round number; an arbitrary one means an input is wrong.
--
-- On the step rather than in a table of its own: a step already means "from this date, the pay
-- is different", and an election change is that. Nullable, because almost every step is a pay
-- rise and nothing else — NULL is "whatever the stream says", not 0%, which is a real election
-- someone can make and must stay expressible.

ALTER TABLE income_stream_steps ADD COLUMN kiwisaver_bps INTEGER
    CHECK (kiwisaver_bps IS NULL OR kiwisaver_bps BETWEEN 0 AND 10000);
ALTER TABLE income_stream_steps ADD COLUMN employer_kiwisaver_bps INTEGER
    CHECK (employer_kiwisaver_bps IS NULL OR employer_kiwisaver_bps BETWEEN 0 AND 10000);
