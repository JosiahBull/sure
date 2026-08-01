-- Per-transaction attribution: an *override* of the account's owner.
--
-- Accounts answer "whose money is this" for the common case. Transactions need to disagree
-- with their account in both directions: a grocery shop on the joint card that was really
-- one person's, and a shared expense someone happened to put on their own card. So this is
-- the same `(ownership, person_id)` pair as `accounts` — parsed into the same `Ownership`
-- enum — with one extra state the account column doesn't have:
--
--   ownership IS NULL             -> inherit from the account (the default, and what every
--                                    imported row gets: a bank feed has no opinion on this)
--   'person' + person_id          -> this one transaction is that person's
--   'joint'  + NULL               -> this one transaction is shared
--
-- NULL meaning "inherit" is what keeps the Akahu/CSV/myIR importers untouched: rows land
-- with no override and follow their account, including retroactively when an account is
-- re-attributed.

ALTER TABLE transactions ADD COLUMN ownership TEXT;
ALTER TABLE transactions ADD COLUMN person_id INTEGER REFERENCES people(id) ON DELETE RESTRICT;

-- Attribution reports group by person over a date range, so the person is the selective
-- column and the date breaks ties within it.
CREATE INDEX idx_tx_person ON transactions(person_id, posted_at);

-- The same invariant the accounts triggers enforce, plus "no person without a discriminant".
-- A table CHECK isn't available (SQLite's ALTER TABLE can't add one), and `transactions` is
-- far too big to rebuild for it.
CREATE TRIGGER transactions_ownership_insert
BEFORE INSERT ON transactions
FOR EACH ROW
WHEN coalesce(NEW.ownership, 'inherit') NOT IN ('person', 'joint', 'inherit')
  OR (NEW.person_id IS NOT NULL) <> (coalesce(NEW.ownership, 'inherit') = 'person')
BEGIN
    SELECT RAISE(ABORT, 'transactions.ownership must be NULL (inherit), ''person'' with a person_id, or ''joint''');
END;

CREATE TRIGGER transactions_ownership_update
BEFORE UPDATE OF ownership, person_id ON transactions
FOR EACH ROW
WHEN coalesce(NEW.ownership, 'inherit') NOT IN ('person', 'joint', 'inherit')
  OR (NEW.person_id IS NOT NULL) <> (coalesce(NEW.ownership, 'inherit') = 'person')
BEGIN
    SELECT RAISE(ABORT, 'transactions.ownership must be NULL (inherit), ''person'' with a person_id, or ''joint''');
END;
