-- Attribution becomes mandatory: every account belongs to a household member or is joint.
-- There is no longer an "unattributed" state to fall into.
--
-- 0015 introduced one because a migration cannot know who owns a pre-existing account, and
-- guessing would be indistinguishable from a real answer. That reasoning still holds — so
-- instead of guessing, this creates a *placeholder* person and points the leftovers at it.
-- The gap is preserved (the row is flagged `placeholder`, and the app keeps asking about it)
-- while the schema gets to be strict from here on: nothing that arrives after this migration
-- can be saved without saying who it belongs to.

-- A person the app created to satisfy the invariant, rather than one anybody named. Cleared
-- the moment that person is renamed through the API — an explicit rename is the answer the
-- placeholder was standing in for.
ALTER TABLE people ADD COLUMN placeholder INTEGER NOT NULL DEFAULT 0;
-- At most one; there is only ever one question being deferred.
CREATE UNIQUE INDEX idx_people_one_placeholder ON people(placeholder) WHERE placeholder = 1;

-- Created when there are accounts with nobody to own them, and also when the household is
-- empty outright: an account can no longer be created without a person to attribute it to,
-- so a database with no people at all would be one where nothing can be added.
INSERT INTO people (name, color, sort_order, placeholder)
SELECT 'Unassigned', NULL, 0, 1
 WHERE NOT EXISTS (SELECT 1 FROM people)
    OR EXISTS (SELECT 1 FROM accounts WHERE ownership = 'unattributed');

UPDATE accounts
   SET ownership = 'person',
       person_id = (SELECT id FROM people WHERE placeholder = 1)
 WHERE ownership = 'unattributed';

-- Same invariant as 0015, minus the state that no longer exists. Recreated rather than
-- edited because SQLite has no ALTER TRIGGER — and 0015 has already run everywhere, so it
-- is not editable either (sqlx checksums it).
DROP TRIGGER accounts_ownership_insert;
DROP TRIGGER accounts_ownership_update;

CREATE TRIGGER accounts_ownership_insert
BEFORE INSERT ON accounts
FOR EACH ROW
WHEN NEW.ownership NOT IN ('person', 'joint')
  OR ((NEW.ownership = 'person') <> (NEW.person_id IS NOT NULL))
BEGIN
    SELECT RAISE(ABORT, 'accounts.ownership must be ''person'' (with a person_id) or ''joint''');
END;

CREATE TRIGGER accounts_ownership_update
BEFORE UPDATE OF ownership, person_id ON accounts
FOR EACH ROW
WHEN NEW.ownership NOT IN ('person', 'joint')
  OR ((NEW.ownership = 'person') <> (NEW.person_id IS NOT NULL))
BEGIN
    SELECT RAISE(ABORT, 'accounts.ownership must be ''person'' (with a person_id) or ''joint''');
END;
