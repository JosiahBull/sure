-- A recurring obligation with a stated amount: rates, insurance, power, internet, a subscription.
-- The deterministic half of a category's spending.
--
-- The premise is that most of what a household spends is contracts rather than behaviour, and the
-- two want opposite treatments. A power bill has no volatility and no fitted trend; it has a price,
-- an escalation clause and sometimes an end date. Fitting a distribution to it describes the
-- billing calendar, not the obligation — and a fixed-term internet plan simply *stopping* in
-- 2027-03 is a behaviour no fitted trend can represent at all.
--
-- Measured on this database before writing any of it: the Utilities category is ~95% one commitment
-- (a $250 power bill, fortnightly, one clean merchant), fitted at a 156%/yr volatility that is
-- entirely an artifact of a $0 billing month sitting beside a $750 one. Same central path either
-- way; the band is the thing this fixes.
--
-- `escalation_delta_bps` is relative to `settings.inflation_bps`, the convention
-- `0043_real_growth_override.sql` established and for the identical reason: "insurance runs ahead
-- of everything else" has to survive a revision of the household rate, and written as an absolute
-- 5.5% it does not.
--
-- `merchant_id` is what makes the netting exact rather than approximate, and it is nullable for a
-- reason that is not laziness — see the two-mechanism split in `sure_app::forecast`'s
-- `CommitmentNetting`. With a merchant, this commitment's own transactions are removed from the
-- series before it is fitted, so the residual's *volatility* narrows too. Without one, only the
-- modelled amount is subtracted from the level and the volatility is unchanged. Both are honest;
-- only the first delivers the band. It must never be both, or the money leaves twice.
CREATE TABLE expense_commitments (
    id                    INTEGER PRIMARY KEY,
    -- RESTRICT, matching `income_streams.linked_category_id` and for the same reason: deleting the
    -- category would un-net the commitment and the residual would silently start double-counting it.
    category_id           INTEGER NOT NULL REFERENCES categories(id) ON DELETE RESTRICT,
    label                 TEXT NOT NULL,
    amount_minor          INTEGER NOT NULL CHECK (amount_minor > 0),
    currency_code         TEXT NOT NULL REFERENCES currencies(code),
    -- see PayFrequency (Rust). Reused rather than re-declared: the only thing read off it here is
    -- `periods_per_year`, that mapping would be a verbatim copy, and both types live in sure-core
    -- so there is no crate boundary for a duplicate to protect. The payday *calendar*
    -- (`payment_counts`) is deliberately not reused — a rates bill has no three-payday months.
    cadence               TEXT NOT NULL,
    first_due_on          TEXT NOT NULL,
    ends_on               TEXT,
    escalation_delta_bps  INTEGER NOT NULL DEFAULT 0
                            CHECK (escalation_delta_bps BETWEEN -2000 AND 2000),
    merchant_id           INTEGER REFERENCES merchants(id) ON DELETE SET NULL,
    enabled               INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    notes                 TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX idx_expense_commitments_category ON expense_commitments(category_id);
CREATE INDEX idx_expense_commitments_merchant ON expense_commitments(merchant_id);
