-- One household inflation rate, in basis points a year, applied to every category the user has
-- not overridden.
--
-- It replaces a *fitted* per-category growth rate, and the reason is that the fit was never
-- estimating what it appeared to. A 24-month window of household spending pins a category's mean
-- to perhaps ±15% and does not identify its trend at all: six of this household's seven fitted
-- categories came out between +43%/yr and +126%/yr — every one of them a level shift after a house
-- purchase, read as a compounding rate — and were clamped to the ±25%/yr derived ceiling. A
-- clamp that six of seven categories sit exactly on is not a guard any more, it is the model, and
-- it is a number nobody chose. Over 360 months it multiplied projected spending by 5.97.
--
-- 250 bps because it is roughly the midpoint of the RBNZ's 1-3% target band, and because the
-- number's job is to be *visible and arguable* rather than right: it appears on the Assumptions
-- tab as one editable figure with the dollars it implies at the horizon printed beneath it, which
-- is a claim a household can disagree with. The clamp it replaces was none of those things.
--
-- NOT NULL with a default rather than nullable: there is no useful distinction between "not set"
-- and "the default", and a nullable column would push a `COALESCE` into every read. The CHECK
-- bounds it at deflation on one side and a rate no household plans around on the other — a
-- fat-fingered 2500 (meaning 25%) is caught here rather than compounding for thirty years.
ALTER TABLE settings
    ADD COLUMN inflation_bps INTEGER NOT NULL DEFAULT 250
        CHECK (inflation_bps BETWEEN -2000 AND 2000);
