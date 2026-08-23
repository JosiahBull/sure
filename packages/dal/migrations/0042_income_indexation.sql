-- Whether a stream's level rises with the household inflation rate once its dated steps run out.
--
-- Expense inflation and income indexation are one decision, not two, and getting only half of it
-- is worse than having neither. `0041_inflation_setting.sql` made every expense category rise at
-- 2.5%/yr; every income stream in this database has `annual_increase_bps = 0`, which
-- `income::level_schedule` reads as a level frozen in nominal terms the moment its last dated step
-- passes — 2033 for a teaching scale, 2035 for two rent ladders, immediately for three others.
-- Inflating one side of the ledger against a frozen other side removes the old overshoot and keeps
-- the deficit: net worth still flattens, just for a new reason.
--
-- Defaults to 0 rather than 1, and that is deliberate even though 1 is what this household wants.
-- Turning it on for existing rows would silently restate everyone's projection on migration, which
-- is the same objection `docs/FORECAST.md` already records against projecting a contribution-driven
-- account at an invented rate: the fix is to say so loudly, not to guess. So the projection instead
-- emits a warning naming every frozen stream and the figure it is frozen at, and the Income tab
-- offers one click to index them.
--
-- When on, `annual_increase_bps` becomes a *real* increase stacked on top of inflation — "CPI + 1%"
-- — which is the same convention a per-category growth override follows, and means the two dials
-- compose instead of contradicting each other. When off it keeps its original nominal meaning, so
-- no stored value changes meaning without the flag moving.
ALTER TABLE income_streams
    ADD COLUMN inflation_indexed INTEGER NOT NULL DEFAULT 0
        CHECK (inflation_indexed IN (0, 1));
