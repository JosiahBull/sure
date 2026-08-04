-- Fold `exchange_rate_cache` back into `exchange_rates`, which is now the only FX table.
--
-- The two were never joined: the background poller (sure_app::tasks::exchange_rates) wrote
-- only `exchange_rate_cache`, while *every* conversion — reports, forecast, the brokerage
-- snapshot, all of it via `sure_app::fx::Fx::load` — read only `exchange_rates`, whose sole
-- writer was config-snapshot import. So on any database that never restored a snapshot
-- carrying rates, `Fx` had no rates at all and silently converted every foreign-currency
-- figure at parity. Polled rates sat in a table nothing read (`list_for_base` had no callers
-- outside its own test).
--
-- `exchange_rates` wins rather than the cache because: it is the one a snapshot round-trips
-- (and `SNAPSHOT_VERSION` is stamped but never validated, so demoting it would let an old
-- snapshot import while quietly discarding its rates); `ExchangeRateRepo::upsert_rate`
-- already hands down an `as_of`, which the cache's (base, quote) PK threw away — it was the
-- table discarding an argument it was given; and a latest-only cache is a strict subset of a
-- dated series, so this is one concept stored twice. Same call `0011_stock_prices.sql` made
-- for prices: a point-in-time lookup has to query by date anyway, so no separate "latest".
--
-- Carry the cached rows over first so a database mid-poll keeps its most recent figures.
-- `DO NOTHING`, not `DO UPDATE`: a row already present for that exact (pair, date) came from
-- a snapshot the user restored, and a restored figure outranks a re-derivable poll. The
-- `WHERE true` is load-bearing — without it SQLite parses `ON` as the start of a JOIN clause
-- and fails with `syntax error near "DO"`. `fetched_at` is dropped with the table; nothing
-- ever read it.
INSERT INTO exchange_rates (base_code, quote_code, as_of, rate)
SELECT base_code, quote_code, as_of, rate
  FROM exchange_rate_cache
 WHERE true
    ON CONFLICT (base_code, quote_code, as_of) DO NOTHING;

-- Dropped here, in the same transaction as the copy, rather than left orphaned: while it
-- exists its rows FK into `currencies`, which snapshot import DELETEs — and since import
-- defers FK checks to COMMIT, any import whose snapshot omitted a currency the cache held a
-- rate for failed at COMMIT with a bare `FOREIGN KEY constraint failed`.
DROP TABLE exchange_rate_cache;
