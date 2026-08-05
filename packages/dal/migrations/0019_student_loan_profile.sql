-- A student loan now has its own metadata profile (`AccountMetadata::StudentLoan`) instead of
-- sharing the `loan` one. The reason is the fields it *stops* carrying: an income-contingent
-- loan is drawn down over years of study, so it has no original principal, no term and no
-- repayment schedule, and while it shared `LoanMeta` a principal was not merely available but
-- *required* — so every student loan created through the account form has an invented figure
-- stored in it, and `sure_app::forecast` would project a fabricated amortisation line the
-- moment a term joined it.
--
-- Reads already survive this without any migration: `metadata_from_stored` coerces the stored
-- `$.profile` to whatever the kind requires before deserialising, and serde ignores keys the
-- target struct doesn't have. So this is a normalisation, and what it buys is that the rows
-- stop *carrying* numbers that no longer mean anything — a principal nobody could have known,
-- a subtype the kind already implies (backfilled by 0014, whose last statement this undoes).
-- Left in place they would read as data to the next SQL query, hand inspection or migration
-- that went looking, which is precisely how an invented figure becomes a trusted one.
--
-- Guards follow 0014: `json_valid`/`json_type` because json_set/json_remove *raise* on a blob
-- that isn't a JSON object rather than returning NULL, and these run at startup
-- (sqlx::migrate! on connect), so an unparseable row would stop the server booting instead of
-- being skipped; `iif(json_valid(metadata), metadata, '{}')` on every read side because the
-- planner may order the WHERE terms however it likes, and iif is a CASE, so its guard holds
-- regardless. The WHERE stops matching once this has run, so a replay is a no-op.
UPDATE accounts
   SET metadata = json_remove(
                    json_set(metadata, '$.profile', 'student_loan'),
                    -- The subtype is implied by the kind; 0014 backfilled it and the new
                    -- profile has no such field.
                    '$.subtype',
                    -- The figure this whole change is about.
                    '$.original_amount_minor',
                    -- A schedule an income-contingent loan does not have. `term_months` and
                    -- `start_date` are the pair docs/STUDENT-LOAN.md warned against setting.
                    '$.rate_type',
                    '$.fixed_until',
                    '$.fixed_term_months',
                    '$.term_months',
                    '$.start_date',
                    '$.refix_rate_bps',
                    '$.refix_rate_uncertainty_bps',
                    -- Repayment is a percentage of income, not an amount on a frequency.
                    '$.repayment_minor',
                    '$.repayment_frequency')
 WHERE kind = 'student_loan'
   AND json_valid(metadata)
   AND json_type(metadata) = 'object'
   AND (json_extract(iif(json_valid(metadata), metadata, '{}'), '$.profile') IS NOT 'student_loan'
     OR json_extract(iif(json_valid(metadata), metadata, '{}'), '$.subtype') IS NOT NULL
     OR json_extract(iif(json_valid(metadata), metadata, '{}'), '$.original_amount_minor') IS NOT NULL
     OR json_extract(iif(json_valid(metadata), metadata, '{}'), '$.rate_type') IS NOT NULL
     OR json_extract(iif(json_valid(metadata), metadata, '{}'), '$.fixed_until') IS NOT NULL
     OR json_extract(iif(json_valid(metadata), metadata, '{}'), '$.fixed_term_months') IS NOT NULL
     OR json_extract(iif(json_valid(metadata), metadata, '{}'), '$.term_months') IS NOT NULL
     OR json_extract(iif(json_valid(metadata), metadata, '{}'), '$.start_date') IS NOT NULL
     OR json_extract(iif(json_valid(metadata), metadata, '{}'), '$.refix_rate_bps') IS NOT NULL
     OR json_extract(iif(json_valid(metadata), metadata, '{}'), '$.refix_rate_uncertainty_bps') IS NOT NULL
     OR json_extract(iif(json_valid(metadata), metadata, '{}'), '$.repayment_minor') IS NOT NULL
     OR json_extract(iif(json_valid(metadata), metadata, '{}'), '$.repayment_frequency') IS NOT NULL);
