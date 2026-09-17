-- Four more historical scales, reaching the table back to the 2019-20 tax year.
--
-- 0050 added 2023-24 and 2024-25 so that an imported salary could be reconciled. The same
-- argument extends as soon as a household records an older job: `scale_for` answers NULL before
-- the earliest scale, and `sure_app::income_match::decompose` then falls back to passing the
-- deposit through whole — gross equal to net, no PAYE, no ACC, no KiwiSaver. That is not a
-- visible failure. The payment still matches and still reconciles; it is the *itemisation* that
-- silently becomes a flat line, so a job from 2020 shows up on the cash-flow chart as if it had
-- been paid untaxed.
--
-- The 39% rate is why this is four rows rather than two. The Taxation (Income Tax Rate and Other
-- Amendments) Act 2020 added the $180,000 / 39% band for the 2021-22 tax year, so 2019-20 and
-- 2020-21 need a bracket table that tops out at 33% — and ESCT, which tracks the PAYE thresholds
-- 20% higher, needs the matching pair. The difference only shows on income most people never
-- reach, which is exactly the kind of error that survives a review; two tables state it instead.
--
-- Figures, all read 2026-09-17:
--
--   2019-04-01  ACC 1.39% to $128,470; student loan 12% over $19,760; no 39% rate.
--   2020-04-01  ACC 1.39% to $130,911; student loan 12% over $20,020; no 39% rate.
--   2021-04-01  ACC 1.39% to $130,911 (unchanged); student loan 12% over $20,280; 39% arrives.
--   2022-04-01  ACC 1.46% to $136,544; student loan 12% over $21,268.
--
-- ACC pairs come from ird.govt.nz's "ACC earners' levy rates" table directly. The loan thresholds
-- come from calculate.co.nz's rates table, trusted because its 2023-24 row reproduces the $22,828
-- that ird.govt.nz states itself — the overlap is the check.
--
-- Same two guards as 0050: skip a year the database already has, however it got there, and do
-- nothing at all on an empty table so a fresh install is seeded from the constants instead.

INSERT INTO tax_scales
    (scale_id, effective_from, brackets, acc_levy_bps, acc_income_cap_minor,
     student_loan_threshold_minor, student_loan_rate_bps, esct_brackets,
     kiwisaver_employer_min_bps, kiwisaver_govt_match_bps, kiwisaver_govt_max_minor,
     kiwisaver_govt_income_cap_minor, source_note)
SELECT * FROM (
    SELECT 'nz_paye' AS scale_id, '2019-04-01' AS effective_from,
           '[[1400000,1050],[4800000,1750],[7000000,3000],[null,3300]]' AS brackets,
           139 AS acc_levy_bps, 12847000 AS acc_income_cap_minor,
           1976000 AS student_loan_threshold_minor, 1200 AS student_loan_rate_bps,
           '[[1680000,1050],[5760000,1750],[8400000,3000],[null,3300]]' AS esct_brackets,
           300 AS kiwisaver_employer_min_bps, 5000 AS kiwisaver_govt_match_bps,
           52143 AS kiwisaver_govt_max_minor, NULL AS kiwisaver_govt_income_cap_minor,
           'Historical scale for the 2019-20 year: ACC 1.39%/$128,470 from ird.govt.nz; student loan 12% over $19,760; pre-39% brackets. Read 2026-09-17.' AS source_note
    UNION ALL
    SELECT 'nz_paye', '2020-04-01',
           '[[1400000,1050],[4800000,1750],[7000000,3000],[null,3300]]',
           139, 13091100, 2002000, 1200,
           '[[1680000,1050],[5760000,1750],[8400000,3000],[null,3300]]',
           300, 5000, 52143, NULL,
           'Historical scale for the 2020-21 year: ACC 1.39%/$130,911 from ird.govt.nz; student loan 12% over $20,020; pre-39% brackets. Read 2026-09-17.'
    UNION ALL
    SELECT 'nz_paye', '2021-04-01',
           '[[1400000,1050],[4800000,1750],[7000000,3000],[18000000,3300],[null,3900]]',
           139, 13091100, 2028000, 1200,
           '[[1680000,1050],[5760000,1750],[8400000,3000],[21600000,3300],[null,3900]]',
           300, 5000, 52143, NULL,
           'Historical scale for the 2021-22 year: the 39% rate over $180,000 begins here; ACC 1.39%/$130,911 unchanged; student loan 12% over $20,280. Read 2026-09-17.'
    UNION ALL
    SELECT 'nz_paye', '2022-04-01',
           '[[1400000,1050],[4800000,1750],[7000000,3000],[18000000,3300],[null,3900]]',
           146, 13654400, 2126800, 1200,
           '[[1680000,1050],[5760000,1750],[8400000,3000],[21600000,3300],[null,3900]]',
           300, 5000, 52143, NULL,
           'Historical scale for the 2022-23 year: ACC 1.46%/$136,544 from ird.govt.nz; student loan 12% over $21,268. Read 2026-09-17.'
) AS s
WHERE NOT EXISTS (
    SELECT 1 FROM tax_scales t
     WHERE t.scale_id = s.scale_id AND t.effective_from = s.effective_from
)
  AND EXISTS (SELECT 1 FROM tax_scales);
