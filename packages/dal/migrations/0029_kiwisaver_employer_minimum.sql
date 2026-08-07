-- The compulsory employer contribution — the least an employer may pay into a contributing
-- member's KiwiSaver account — as a dated, editable figure rather than a constant in the binary.
--
-- It moves: 3% until 31 March 2026, 3.5% from 1 April 2026, 4% from 1 April 2028. Until now it
-- lived as `sure_core::tax::KIWISAVER_DEFAULT_BPS`, which is undated, so a projection spanning
-- 1 April 2026 priced both sides of the step at whichever figure the binary happened to carry.

ALTER TABLE tax_scales ADD COLUMN kiwisaver_employer_min_bps INTEGER NOT NULL DEFAULT 300
    CHECK (kiwisaver_employer_min_bps BETWEEN 0 AND 10000);

-- Backfill by date. `tax_scales::seed` is the single place the figures are written down and it
-- deliberately never touches a non-empty table, so it cannot fix the rows an existing install
-- already has — leaving every one of them at the 3% default, including the 2026-04-01 scale where
-- 3.5% is the whole point. Keyed to the statutory date rather than to the seeded rows, so a scale
-- the user added themselves for a future year is backfilled correctly too. A fresh database is
-- seeded from the constants after migrating and never reaches this statement with rows to update.
UPDATE tax_scales SET kiwisaver_employer_min_bps = 350
 WHERE scale_id = 'nz_paye' AND effective_from >= '2026-04-01';
