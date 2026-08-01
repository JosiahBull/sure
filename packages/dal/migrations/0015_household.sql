-- Household individuals, and which of them an account belongs to.
--
-- `people` is a table rather than a fixed two-person enum: the names are user data, the
-- same way `merchants`/`categories` are. Two rows is the expected household, but nothing
-- above this depends on that.
--
-- Ownership is the `(ownership, person_id)` column pair, parsed into one `Ownership` enum
-- (sure-core) the moment a row is read. Three states, and the third one matters:
--
--   'person'       + person_id  -> belongs to that individual
--   'joint'        + NULL       -> shared by the household (a joint account, the family home)
--   'unattributed' + NULL       -> not yet attributed
--
-- Every existing row lands on 'unattributed', deliberately. This migration cannot know
-- which accounts are one person's and which are shared, and a guess would be
-- indistinguishable from a real answer once every per-person figure is derived from it --
-- the same reasoning 0014 gives for refusing to invent a lender or a city. The app surfaces
-- the unattributed set and asks; it never fills it in silently.

CREATE TABLE people (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    color       TEXT,                           -- badge/chart colour; NULL => derived from id
    sort_order  INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
-- Two people called "Sam" in one household is a typo, not a household.
CREATE UNIQUE INDEX idx_people_name ON people(name COLLATE NOCASE);

-- RESTRICT, not SET NULL: deleting a person who still owns accounts would silently turn
-- their whole net worth into "unattributed". The DAL refuses the delete with a 409 naming
-- the counts long before this fires; this is the backstop for anything that goes around it.
ALTER TABLE accounts ADD COLUMN ownership TEXT NOT NULL DEFAULT 'unattributed';
ALTER TABLE accounts ADD COLUMN person_id INTEGER REFERENCES people(id) ON DELETE RESTRICT;
CREATE INDEX idx_accounts_person ON accounts(person_id);

-- The invariant is "person_id is set exactly when ownership = 'person'". It cannot be a
-- table CHECK: SQLite's ALTER TABLE adds columns but never table-level constraints, and
-- rebuilding `accounts` (the FK target of six other tables) to gain one is not worth it.
-- Triggers say the same thing and run on every writer, including a hand-edited row.
-- sure-core's `Ownership` is the real enforcement; these keep the schema honest on its own.
CREATE TRIGGER accounts_ownership_insert
BEFORE INSERT ON accounts
FOR EACH ROW
WHEN NEW.ownership NOT IN ('person', 'joint', 'unattributed')
  OR ((NEW.ownership = 'person') <> (NEW.person_id IS NOT NULL))
BEGIN
    SELECT RAISE(ABORT, 'accounts.person_id must be set exactly when ownership = ''person''');
END;

CREATE TRIGGER accounts_ownership_update
BEFORE UPDATE OF ownership, person_id ON accounts
FOR EACH ROW
WHEN NEW.ownership NOT IN ('person', 'joint', 'unattributed')
  OR ((NEW.ownership = 'person') <> (NEW.person_id IS NOT NULL))
BEGIN
    SELECT RAISE(ABORT, 'accounts.person_id must be set exactly when ownership = ''person''');
END;
