-- The debit half of the interest pair 0026 only shipped the credit half of.
--
-- 'Bank interest paid → Interest earned' (priority 5) matches 'cr.int' and 'credit int' —
-- interest a bank pays you. A bank writes the charge the same way with one letter changed:
--
--     CR.INT TO 01/09/2025      interest earned, caught
--     DR.INT TO 01/09/2025      interest charged, fell through
--
-- Nothing distinguished these but the missing rule, so on a real ledger 49 credits were
-- classified and every debit was not — 8 rows, monthly on a revolving credit facility, for as
-- long as the facility is drawn. The asymmetry was an oversight rather than a decision: there
-- is no sense in which a bank charging interest is less classifiable than a bank paying it.
--
-- 'Interest charged' rather than 'Interest earned', obviously, and the same priority as the
-- sibling so the pair sits together. The two cannot collide — 'dr.int' is not a substring of
-- 'cr.int' and vice versa — so neither steals the other's rows whichever is reached first.
--
-- 'debit int' rides along to mirror the sibling's 'credit int', for a bank that spells it out.
-- Written in the form the web rule builder emits, so it opens as one editable condition.

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Bank interest charged → Interest charged',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''dr.int'') or contains(lower(description), ''debit int''))',
       (SELECT id FROM categories WHERE name = 'Interest charged'),
       0, 1, 5, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Bank interest charged → Interest charged');
