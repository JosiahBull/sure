-- The tax scales for 2023-24 and 2024-25, so the matcher can price a deposit that already landed.
--
-- `tax_scales::seed` writes `sure_core::tax::NZ_TAX_SCALES` into an empty table and, deliberately,
-- never touches a table with rows in it — so adding a scale to the constants fixes a fresh install
-- and does nothing for anyone who already has one. 0029 hit the same wall and solved it the same
-- way: state the figures here as well, keyed so they cannot land twice.
--
-- Why it matters, and it is not a projection concern. `scale_for` answers `None` for a date before
-- every scale on record — correct, and documented as such: rules that had not been written yet are
-- not a thing to guess at. `sure_app::income_match::expected_net` then falls back to predicting the
-- *gross*, which misses the net deposit by about a third, so every pay before the earliest scale is
-- reported as never paid no matter how well the stream is configured. A household that imports
-- three years of bank history has three years of salary in it, and until this migration the first
-- two of them could not be reconciled.
--
-- The figures, all read 2026-09-17, and each reproduced against real deposits before being written
-- here: inverting a run of pays back to an annual gross lands on the same round salary under all
-- three of these scales, including across the 31 July 2024 threshold change — which raised
-- take-home without the pay moving, and is therefore the sharpest test of the pair of scales
-- either side of it:
--
--   2023-04-01  ACC 1.53% to $139,384; student loan 12% over $22,828; pre-Budget-2024 brackets.
--   2024-04-01  ACC 1.60% to $142,283; student loan 12% over $24,128 (frozen there since).
--   2024-07-31  Budget 2024's thresholds, which took effect mid-tax-year, hence the odd date.
--               Levy, cap and loan unchanged from the row above; only the brackets move.
--
-- ESCT keeps its pre-2025 table across all three: it reset on 1 April 2025, a year after the PAYE
-- thresholds moved, and pairing the new PAYE table with the new ESCT one for 2024-25 would
-- overstate what reached a KiwiSaver account that year.
--
-- `kiwisaver_govt_income_cap_minor` is NULL for "no income test", which is how the years before
-- Budget 2025 worked; the 50c-per-dollar match to $521.43 is the pre-Budget-2025 contribution.

INSERT INTO tax_scales
    (scale_id, effective_from, brackets, acc_levy_bps, acc_income_cap_minor,
     student_loan_threshold_minor, student_loan_rate_bps, esct_brackets,
     kiwisaver_employer_min_bps, kiwisaver_govt_match_bps, kiwisaver_govt_max_minor,
     kiwisaver_govt_income_cap_minor, source_note)
SELECT * FROM (
    SELECT 'nz_paye' AS scale_id, '2023-04-01' AS effective_from,
           '[[1400000,1050],[4800000,1750],[7000000,3000],[18000000,3300],[null,3900]]' AS brackets,
           153 AS acc_levy_bps, 13938400 AS acc_income_cap_minor,
           2282800 AS student_loan_threshold_minor, 1200 AS student_loan_rate_bps,
           '[[1680000,1050],[5760000,1750],[8400000,3000],[21600000,3300],[null,3900]]' AS esct_brackets,
           300 AS kiwisaver_employer_min_bps, 5000 AS kiwisaver_govt_match_bps,
           52143 AS kiwisaver_govt_max_minor, NULL AS kiwisaver_govt_income_cap_minor,
           'Historical scale added so already-imported pay can be reconciled: ACC 1.53%/$139,384 and student loan 12% over $22,828 from ird.govt.nz''s 2023-24 rates; brackets as in force since 1 Oct 2010. Read 2026-09-17.' AS source_note
    UNION ALL
    SELECT 'nz_paye', '2024-04-01',
           '[[1400000,1050],[4800000,1750],[7000000,3000],[18000000,3300],[null,3900]]',
           160, 14228300, 2412800, 1200,
           '[[1680000,1050],[5760000,1750],[8400000,3000],[21600000,3300],[null,3900]]',
           300, 5000, 52143, NULL,
           'Historical scale added so already-imported pay can be reconciled: ACC 1.60%/$142,283 and student loan 12% over $24,128 from ird.govt.nz''s 2024-25 rates. Read 2026-09-17.'
    UNION ALL
    SELECT 'nz_paye', '2024-07-31',
           '[[1560000,1050],[5350000,1750],[7810000,3000],[18000000,3300],[null,3900]]',
           160, 14228300, 2412800, 1200,
           '[[1680000,1050],[5760000,1750],[8400000,3000],[21600000,3300],[null,3900]]',
           300, 5000, 52143, NULL,
           'Historical scale added so already-imported pay can be reconciled: Budget 2024''s thresholds, in force for PAYE from 31 July 2024; levy, cap, loan and ESCT unchanged from the 2024-04-01 row. Read 2026-09-17.'
) AS s
-- Idempotent against a database that already has one of these, however it got there: a user may
-- have typed a 2023-24 scale in themselves, and theirs is the authority over a seeded default.
WHERE NOT EXISTS (
    SELECT 1 FROM tax_scales t
     WHERE t.scale_id = s.scale_id AND t.effective_from = s.effective_from
)
-- **And only on a database that already has scales.** Migrations run before `tax_scales::seed`,
-- and that function writes the built-in table only when it finds *no* rows at all. Insert into an
-- empty table here and seeding sees three rows, concludes the table is populated, and returns —
-- leaving a brand-new install with the two historical scales and none of the current ones. The
-- guard splits the two cases cleanly: a fresh database is empty here, does nothing, and is seeded
-- from the constants (which carry these three rows as well); an existing one is not empty, and
-- gets exactly the rows its seeding predates.
  AND EXISTS (SELECT 1 FROM tax_scales);
