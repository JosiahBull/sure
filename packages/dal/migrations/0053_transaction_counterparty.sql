-- Where a transaction's money went, when the other side is an account rather than a payee.
--
-- `linked_transaction_id` already answers this — but only when the far side *has a
-- transaction to link to*, and that is not the common case:
--
--   * a property or a vehicle is valuation-backed and has no transaction stream at all
--     (see `opening_balance_ledger` in `accounts.rs`), so a $77,000 house deposit can never
--     be linked to the house it bought;
--   * a loan's interest is charged to the facility the payment is drawn from rather than to
--     the loan, so the row that *is* the cost of a mortgage has no counterpart anywhere;
--   * `link_transfers` pairs on an exactly opposite amount, so a balance-only account whose
--     derived rows are net deltas never matches the payment that moved it.
--
-- One nullable column closes all three. It is an *override*, not a replacement: reports read
-- it first and fall back to the linked transaction's account, so every pair the auto-linker
-- finds keeps working untouched and nothing is denormalised into two places that can drift.
--
-- `ON DELETE SET NULL`, like `merchant_id` and `category_id`: deleting an account must not
-- take a year of history with it, and a transaction that has lost its counterparty is still
-- a transaction — it has just stopped saying where the money went.
ALTER TABLE transactions ADD COLUMN counterparty_account_id INTEGER REFERENCES accounts(id) ON DELETE SET NULL;

-- The cashflow report groups a window's rows by the account they crossed to, so the account
-- is the selective column and the date narrows within it — the same shape as `idx_tx_person`.
CREATE INDEX idx_tx_counterparty ON transactions(counterparty_account_id, posted_at);

-- A rule can assign one, because the rows that need it recur forever: a mortgage charges its
-- interest every fortnight, and tagging each new one by hand is the kind of upkeep that stops
-- happening after a month.
ALTER TABLE rules ADD COLUMN set_counterparty_account_id INTEGER REFERENCES accounts(id) ON DELETE SET NULL;

-- Before/after, so a rule run stays undoable — the same pair every other rule action has.
ALTER TABLE rule_applications ADD COLUMN prev_counterparty_account_id INTEGER;
ALTER TABLE rule_applications ADD COLUMN new_counterparty_account_id INTEGER;
