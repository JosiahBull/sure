-- What a fund charges to hold your money.
--
-- Fees belong here, beside growth and volatility, because that is what a fee *is*: a deduction from
-- the return an account earns. Putting them on the account's metadata would have been the other
-- obvious home, but then the forecast would have to reach into a JSON blob for something it needs
-- every month of every path, and two different profiles would each need their own copy of the same
-- two fields.
--
-- They matter most for exactly the accounts 0023 just started routing contributions into. A
-- KiwiSaver fund charging 1.05% a year against a 6% gross return keeps 4.95% — over thirty years
-- that is about a quarter of the final balance, which is not a rounding error and is invisible
-- unless it is modelled. NULL means "not modelled", not "zero": a fund with no fee is a claim worth
-- making deliberately, and a projection that silently assumes one is flattering.
ALTER TABLE forecast_assumptions ADD COLUMN annual_fee_bps INTEGER
    CHECK (annual_fee_bps IS NULL OR annual_fee_bps BETWEEN 0 AND 10000);

-- The flat annual membership/administration fee some funds charge on top of the percentage — a
-- few tens of dollars, which is trivial against a large balance and is not against a small one.
-- In the account's own currency, minor units.
ALTER TABLE forecast_assumptions ADD COLUMN annual_fixed_fee_minor INTEGER
    CHECK (annual_fixed_fee_minor IS NULL OR annual_fixed_fee_minor >= 0);
