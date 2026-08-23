-- Whether a category's growth override is a rate *above inflation* rather than an absolute one.
--
-- The same convention `income_streams.inflation_indexed` uses, and adopted here for the same
-- reason: with one household rate in play, a per-category opinion is almost always a claim about
-- how that category behaves *relative* to everything else. Childcare, rates and insurance
-- genuinely outrun inflation; a mortgage-adjacent fee tracks it; a subscription someone is about
-- to cancel does not. Written as "CPI + 1%" those three stay correct when the household rate
-- moves, and written as "3.5%" all three silently become wrong the moment it does.
--
-- That is the whole argument. `settings.inflation_bps` is a number people will revise — it is on
-- the Assumptions tab precisely so it can be argued with — and an absolute per-category override
-- is a copy of it that does not get revised with it. One edit should re-price the plan coherently.
--
-- Defaults to 0 (absolute), so every override already stored keeps meaning exactly what it meant.
-- Only consulted for a category: an account's growth is a market return, which is not a spread
-- over household CPI and should not be expressible as one.
ALTER TABLE forecast_assumptions
    ADD COLUMN growth_is_real INTEGER NOT NULL DEFAULT 0
        CHECK (growth_is_real IN (0, 1));
