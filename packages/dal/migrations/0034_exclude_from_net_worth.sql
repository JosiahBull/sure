-- Let an account be kept out of the household's net worth without hiding it.
--
-- Some balances are real, and yours to see, without being part of what you are worth: money
-- held for someone else, a company account that shares a login, a pot you track but do not
-- count. Until now the only lever was `archived`, which removes the account from the app
-- entirely — a different thing, and useless when you still want the balance in front of you.
--
-- Named for the exclusion rather than the inclusion so that `0`, the value every existing row
-- silently receives, is the do-nothing state: every figure the app reports is unchanged by
-- this migration, which is what makes it safe to apply to a live ledger.
--
-- A boolean and not a TEXT enum: CLAUDE.md rule 1 governs values with a closed set of *named
-- alternatives*, and this has two states with no third candidate — the same call `archived`
-- made in 0001. If a second axis ever arrives ("out of the forecast but in net worth"), that
-- is a new column, and rule 1 applies to it then.
--
-- `INTEGER NOT NULL DEFAULT 0` because `accounts` is STRICT and SQLite's ALTER TABLE ADD
-- COLUMN only accepts a constant default.

ALTER TABLE accounts ADD COLUMN excluded_from_net_worth INTEGER NOT NULL DEFAULT 0;
