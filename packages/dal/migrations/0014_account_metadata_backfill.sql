-- Account metadata is now required on save (see AccountMetadata::validate_for): a property has
-- to name its subtype/city/country, a mortgage its lender and principal, an investment its
-- broker. Existing rows predate that, so this fills in only what a row *already implies* and
-- nothing else.
--
-- Derivation only, deliberately: a guessed lender or an invented city would be indistinguishable
-- from a real answer once stored, and every figure derived from it would then look trustworthy.
-- Every remaining gap is left blank on purpose, so the account's next save is a 422 and the form
-- asks its owner for the real value.
--
-- Each statement's WHERE stops matching once it has run, so re-running this against an
-- already-migrated database is a no-op. `metadata` is read through
-- `iif(json_valid(metadata), metadata, '{}')` because json_extract *raises* on a blob that
-- isn't JSON (a hand-edited row) rather than returning NULL; iif is a CASE, so the guard holds
-- however the planner orders the terms, and a row we can't parse is simply skipped.
--
-- `json_valid(metadata)` is then required by every WHERE as well, because the iif only protects
-- the *read*: json_set on the SET side raises on the same blob, and these run at startup
-- (sqlx::migrate! on connect), so an unparseable row would stop the server from booting rather
-- than being skipped.

-- Properties: the address used to be a single `address` key. The Rust side still reads that
-- through a serde alias, but move the stored key over too so the row matches what we now write.
UPDATE accounts
   SET metadata = json_remove(
                    json_set(metadata, '$.address_line1', json_extract(metadata, '$.address')),
                    '$.address')
 WHERE kind = 'real_estate'
   AND json_valid(metadata)
   AND json_extract(iif(json_valid(metadata), metadata, '{}'), '$.address') IS NOT NULL
   AND json_extract(iif(json_valid(metadata), metadata, '{}'), '$.address_line1') IS NULL;

-- A row that already carries *both* keys (e.g. `address_line1` was filled in by hand without
-- clearing the old `address`) is worse than the single-key case above: serde's
-- `#[serde(alias = "address")]` treats the two as one field and rejects the duplicate, and
-- `metadata_from_stored` then falls back to an *empty* value for the whole property rather
-- than just the address, so a row like this reads back with no subtype, no city, nothing.
-- Only `address` is dropped here — `address_line1`, the current key, is left exactly as it
-- is — so this is removing a redundant duplicate, not guessing a value, and stays inside this
-- migration's derivation-only remit.
UPDATE accounts
   SET metadata = json_remove(metadata, '$.address')
 WHERE kind = 'real_estate'
   AND json_valid(metadata)
   AND json_extract(iif(json_valid(metadata), metadata, '{}'), '$.address') IS NOT NULL
   AND json_extract(iif(json_valid(metadata), metadata, '{}'), '$.address_line1') IS NOT NULL;

-- Borrowing: the lender is the institution the account already records.
--
-- `json_type(metadata) = 'object'` is required alongside `json_valid(metadata)`: a
-- valid-but-non-object blob (e.g. `'[]'`) has no `$.lender` key either way, so
-- `json_extract(...) IS NULL` would be true forever and this UPDATE would re-fire as a no-op
-- write on every replay instead of actually stopping once it's run.
UPDATE accounts
   SET metadata = json_set(metadata, '$.lender', institution)
 WHERE kind IN ('mortgage', 'student_loan', 'loan')
   AND json_valid(metadata)
   AND json_type(metadata) = 'object'
   AND trim(coalesce(institution, '')) <> ''
   AND json_extract(iif(json_valid(metadata), metadata, '{}'), '$.lender') IS NULL;

-- Investments: likewise, the broker is the platform already named as the institution.
UPDATE accounts
   SET metadata = json_set(metadata, '$.broker', institution)
 WHERE kind IN ('brokerage', 'shares_nz', 'shares_us', 'shares_private')
   AND json_valid(metadata)
   AND json_type(metadata) = 'object'
   AND trim(coalesce(institution, '')) <> ''
   AND json_extract(iif(json_valid(metadata), metadata, '{}'), '$.broker') IS NULL;

-- A student loan's loan subtype is implied by the kind: it can only be 'student'.
UPDATE accounts
   SET metadata = json_set(metadata, '$.subtype', 'student')
 WHERE kind = 'student_loan'
   AND json_valid(metadata)
   AND json_type(metadata) = 'object'
   AND json_extract(iif(json_valid(metadata), metadata, '{}'), '$.subtype') IS NULL;
