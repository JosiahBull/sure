-- Catch StudyLink payments under the abbreviations ASB actually writes.
--
-- 0026's 'Student loan drawdowns and deductions' rule already means to cover these: it looks
-- for 'living costs' and 'course related costs'. But that is the wording the *scheme* uses,
-- not the wording that reaches a statement. ASB renders both as initials —
--
--     D/C FROM STUDYLINK (MSD) LC PAYMENT REF:…      living costs
--     D/C FROM STUDYLINK (MSD) CRC PAYMENT REF:…     course related costs
--
-- — so neither token matches and every StudyLink payment falls through uncategorised. On a
-- real ledger that was 145 rows and $44k, none of them caught by any shipped rule, and they
-- keep arriving fortnightly for as long as someone is studying.
--
-- Both are student *loan* components (the loan is drawn down to pay living costs and course
-- costs), so they classify as 'Student loan' — the same category, and the same `transfer`
-- treatment, the sibling rule gives the spelled-out forms. A Student Allowance is a grant
-- rather than a drawdown and would be genuine income, which is why this is keyed to the two
-- payment types and not to the payer: 'studylink' alone would sweep an allowance in with them.
--
-- 'lc paym'/'crc paym' rather than 'lc payment': one real variant runs the fields together as
-- "LC PAYMENTREF:…", and the shorter token has more room to survive ASB's twelve-character
-- memo split. They are short enough to appear inside ordinary words, which is what the
-- 'studylink' half of the `and` is for — neither side is safe alone.
--
-- StudyLink is a government agency, so this ships for the same reason 'inland revenue',
-- 'acc levy', 'watercare' and 'nz transport' do in 0026, and unlike a named employer (rule 3).
--
-- Written in the form the web rule builder emits, so it opens as two editable conditions
-- rather than "Custom conditions". Priority 3 puts it beside the sibling rule it completes.

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'StudyLink living and course costs → Student loan',
       'Default rule shipped with Sure.',
       'contains(lower(description), ''studylink'') and (contains(lower(description), ''lc paym'') or contains(lower(description), ''crc paym''))',
       (SELECT id FROM categories WHERE name = 'Student loan'),
       0, 1, 3, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'StudyLink living and course costs → Student loan');
