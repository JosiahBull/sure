-- Categorise the interest a mortgage or personal loan charges itself.
--
-- 0026 shipped two rules for loan interest, both keyed to the wording a *bank statement*
-- uses when a repayment leaves a transaction account: "LOAN REPAYMENT <account> INTEREST"
-- and the matching "... PRINCIPAL". Neither fires on the wording the loan account's own
-- feed posts, which is a single row reading "Interest of $1083.51 Principal" — no "loan
-- repayment" anywhere in it, so the `and` in both rules fails and the charge lands
-- uncategorised. It is a monthly row on every mortgage and loan, so it never stops arriving.
--
-- Keyed on `account_kind` as well as the wording, because "interest of ..." is generic
-- enough to want the account to vouch for it: on a mortgage or a loan the row is money
-- charged, which is what makes 'Interest charged' the right side of the ledger. The same
-- words on a savings account would be interest *earned*, and that case already has its own
-- rule ('Bank interest paid → Interest earned', priority 5) matching different wording.
--
-- Priority 0 puts it beside the sibling rule it completes; evaluation is `ORDER BY
-- priority, id`, so it is reached after that one, and only the rows that one leaves alone
-- can get here.
--
-- Written in exactly the form the web rule builder emits — `account_kind in [...]` for
-- "Account type is any of", `startsWith(lower(description), ...)` for "Description starts
-- with", joined by a bare `and` — so the builder parses it back into two editable conditions
-- rather than showing "Custom conditions" and refusing to open. A shipped rule is the first
-- one a person opens to see how rules work; it should not be the one the editor can't read.
--
-- Conditional like everything in 0026, so this is a no-op on a database that already has it.

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Loan account interest charged → Interest charged',
       'Default rule shipped with Sure.',
       'account_kind in [''mortgage'', ''loan''] and startsWith(lower(description), ''interest of'')',
       (SELECT id FROM categories WHERE name = 'Interest charged'),
       0, 1, 0, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Loan account interest charged → Interest charged');
