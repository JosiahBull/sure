-- Stop the university rule from filing a university *salary* as a tertiary-education expense.
--
-- 0026's 'Universities → Tertiary education' matches the wording regardless of direction, and
-- a university is an employer as readily as it is a place you pay fees to. On a real ledger it
-- claimed three "UNI OF AUCKLAND SALARY" credits — income — and filed them under an expense
-- category, so the same rows both understated income and inflated education spending.
--
-- `and is_expense` is the narrowest fix: the rule keeps every token it had and only stops
-- looking at money coming *in*.
--
-- The known cost, stated because it is a real trade and not an oversight: a genuine tuition
-- *refund* is also money coming in, and it will now land uncategorised instead of netting off
-- the fees it reverses. That is the better failure of the two — an uncategorised refund is
-- visible on the dashboard and takes one click, whereas a salary silently booked as an expense
-- corrupts both sides of the report and looks like nothing is wrong.
--
-- Deliberately NOT applied to the other shipped expense rules, though several also hold
-- positive rows. They are not the same bug and a blanket `is_expense` would break them:
-- 'Loan account interest charged' (0030) matches interest *charged to a liability*, which is
-- positive by construction — the balance grows — and would stop matching entirely; and
-- refunds/withdrawals that legitimately belong against a spend category (a broker withdrawal,
-- a retail return) would be pushed out of the category that nets them off. Direction is only
-- decisive where the same words describe two different relationships, which is what makes an
-- employer-or-vendor like a university the case that needs it.
--
-- An UPDATE, since 0026 has run everywhere and migrations are append-only. Guarded on the
-- expression as well as the name, so a rule someone has edited themselves keeps their version
-- rather than being silently reverted to ours.
--
-- Existing misfiled rows are left alone: re-running the rules reclassifies them, and deciding
-- that for someone inside a migration would also catch the refunds this cannot tell apart.

UPDATE rules
   SET expression = '(contains(lower(description), ''uni of auckl'') or contains(lower(description), ''university o'') or contains(lower(description), ''ak uni'') or contains(lower(description), ''academic dre'') or contains(lower(description), ''uoa'') or contains(lower(description), ''canterbury u'') or contains(lower(description), ''language tra'')) and is_expense',
       updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
 WHERE name = 'Universities → Tertiary education'
   AND expression = '(contains(lower(description), ''uni of auckl'') or contains(lower(description), ''university o'') or contains(lower(description), ''ak uni'') or contains(lower(description), ''academic dre'') or contains(lower(description), ''uoa'') or contains(lower(description), ''canterbury u'') or contains(lower(description), ''language tra''))';
