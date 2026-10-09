-- Correct the rule 0030 shipped: on a loan's own feed that row is the *principal*, not the
-- interest.
--
-- 0030 read "Interest of $1083.51 Principal" — the single row a mortgage's own Akahu feed posts
-- against each repayment — as the interest being charged, and filed it under 'Interest charged'.
-- The memo names both figures, but the row's *amount* is the principal, and the evidence is not
-- ambiguous: across nineteen consecutive payments on a real ASB mortgage each row matched, to the
-- cent, the `LOAN REPAYMENT … 006PRINCIPAL` debit on the facility the payment was drawn from —
-- while the interest the memo names matched the separate `… 006INTEREST` debit beside it. The
-- interest never touches the loan account at all; ASB charges it to the facility.
--
-- So the rule was filing a transfer of principal as a spending expense. Nothing *reported* it —
-- a mortgage is outside the cash perimeter, so its own rows feed no income or expense total —
-- which is exactly why it went unnoticed for months. It was wrong on the account page, and a
-- standing trap for the next thing to read those categories.
--
-- Only the destination changes; the wording it matches is right, and `stop_on_match` stays, so
-- this keeps beating any later rule that would file the same row differently.
UPDATE rules
   SET name = 'Loan account principal payment → Transfer',
       set_category_id = (SELECT id FROM categories WHERE name = 'Transfer'),
       updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
 WHERE name = 'Loan account interest charged → Interest charged'
   AND EXISTS (SELECT 1 FROM categories WHERE name = 'Transfer');

-- The same payment, in the wording Akahu began sending in September 2026: a bare
-- "Principal payment", with the figures gone. The older rule cannot match it — it keys on
-- "interest of" — so without this one a fresh install files every new repayment nowhere.
--
-- Guarded on the *expression* rather than the name, because a household that hit this first will
-- already have written the rule by hand and does not need a second one aimed at the same rows.
-- Written in the form the web rule builder parses (`account_kind in [...]` and an equality on
-- `lower(description)`, joined by a bare `and`) for the reason 0030 gives: a shipped rule is the
-- first one somebody opens to see how rules work.
INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Loan principal payment → Transfer',
       'Default rule shipped with Sure.',
       'account_kind in [''mortgage'', ''loan''] and lower(description) == ''principal payment''',
       (SELECT id FROM categories WHERE name = 'Transfer'),
       0, 1, 0, 1
WHERE EXISTS (SELECT 1 FROM categories WHERE name = 'Transfer')
  AND NOT EXISTS (SELECT 1 FROM rules WHERE lower(expression) LIKE '%principal payment%');
