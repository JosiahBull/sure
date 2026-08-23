-- A price ledger for unlisted holdings, so a private company's share price is a time series
-- rather than one mutable number.
--
-- `equity_grants.unit_value_minor` was a single scalar per grant, which made two things
-- impossible. A grant could not say what a unit was worth *then* as distinct from now, so
-- valuing a past date used today's mark — a 2024 position priced at a 2026 round. And repricing
-- after a funding round meant editing every grant on the account, with no record that the old
-- price had ever applied and no way to see when it changed.
--
-- Quantity was never the problem: vested/exercised at any date is already derived exactly from
-- the grant schedule. Splitting the price out is what makes an account's whole valuation history
-- computable — quantity(t) x mark(t) — instead of hand-entered.
--
-- Marks key on the *account*, not the grant: every grant on one account is equity in the same
-- company, and a per-grant price would let two grants disagree about what one share is worth.
-- (Logically the owner is the company. If an account ever needs to hold grants from two
-- companies, that is the point to move this key, not before.)
CREATE TABLE equity_marks (
    id               INTEGER PRIMARY KEY,
    account_id       INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    as_of            TEXT NOT NULL,               -- ISO-8601 date; the mark applies from here on
    unit_value_minor INTEGER NOT NULL,            -- fair value of one unit, minor units
    currency_code    TEXT NOT NULL REFERENCES currencies(code),
    note             TEXT,
    created_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- One mark per account per day: a mark is a *level* that carries forward until the next one
-- (the same contract `valuations` has), so two on one date have no defined order. This is what
-- lets a re-import or a corrected figure upsert instead of silently stacking.
CREATE UNIQUE INDEX idx_equity_marks_account_date ON equity_marks(account_id, as_of);

-- Carry every existing account's current price into the ledger so nothing changes value on
-- upgrade. Dated at the account's earliest grant, because that is the furthest back the figure
-- could have applied and it keeps a pre-existing account's history exactly as it reads today.
--
-- One mark per account, from the most recently created grant that carries a price: grants on one
-- account are meant to agree, and where they have drifted apart the newest is the one last
-- edited. Anything older than the last funding round has to be entered by hand — this migration
-- cannot know what a share was worth in 2024, and inventing a curve would be worse than a flat
-- line the owner can see and correct.
INSERT INTO equity_marks (account_id, as_of, unit_value_minor, currency_code, note)
SELECT g.account_id,
       (SELECT MIN(grant_date) FROM equity_grants WHERE account_id = g.account_id),
       g.unit_value_minor,
       g.currency_code,
       'carried over from the grant''s unit value when the mark ledger was added'
  FROM equity_grants g
 WHERE g.unit_value_minor IS NOT NULL
   AND g.id = (SELECT MAX(id) FROM equity_grants
                WHERE account_id = g.account_id AND unit_value_minor IS NOT NULL);
