//! Brokerage compute: turn the holdings ledger + wallet cash into a valued snapshot
//! (pricing each position via the historical stock-price cache and converting into the
//! account's currency), snapshot it into a valuation, and backfill a daily valuation
//! series across the account's whole history. Persistence goes through the
//! [`AccountRepo`]/[`BrokerageRepo`]/[`StockPriceCacheRepo`]/[`ValuationRepo`]/[`FxRatesRepo`]
//! ports (implemented by `sure-dal`'s `SqliteStore`); this module is the price-lookup +
//! FX orchestration, matching the `price_at`/`StockPriceTask` split in `crate::stock_prices`.

use std::sync::Arc;

use chrono::NaiveDate;

use std::collections::HashMap;

use sure_core::StockPrice;
use sure_core::{
    AppError, AppResult, BrokerageActivity30d, BrokerageSnapshot, LotKind, Position, WalletBalance,
};

use crate::fx::Fx;
use crate::ports::{
    AccountRepo, BrokerageRepo, Clock, CostLotRow, FxRatesRepo, StockPriceCacheRepo,
    StockPriceProvider, ValuationRepo,
};

pub struct BrokerageService {
    accounts: Arc<dyn AccountRepo>,
    brokerage: Arc<dyn BrokerageRepo>,
    prices: Arc<dyn StockPriceCacheRepo>,
    valuations: Arc<dyn ValuationRepo>,
    fx: Arc<dyn FxRatesRepo>,
    clock: Arc<dyn Clock>,
}

impl BrokerageService {
    pub fn new(
        accounts: Arc<dyn AccountRepo>,
        brokerage: Arc<dyn BrokerageRepo>,
        prices: Arc<dyn StockPriceCacheRepo>,
        valuations: Arc<dyn ValuationRepo>,
        fx: Arc<dyn FxRatesRepo>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            accounts,
            brokerage,
            prices,
            valuations,
            fx,
            clock,
        }
    }

    /// Resolve a position's price as of `as_of`. With a provider, backfill the cache from
    /// it on a miss (the live endpoints want fresh data); without one, read cache-only
    /// (the day-by-day backfill loop, after a single bulk fetch has already warmed the
    /// cache — so it never fires one upstream request per day).
    async fn resolve_price(
        &self,
        provider: Option<&dyn StockPriceProvider>,
        ticker: &str,
        exchange: &str,
        as_of: NaiveDate,
    ) -> AppResult<Option<StockPrice>> {
        match provider {
            Some(p) => {
                crate::stock_prices::price_at(self.prices.as_ref(), p, ticker, exchange, as_of)
                    .await
            }
            None => {
                self.prices
                    .get_at(ticker, exchange, &as_of.to_string())
                    .await
            }
        }
    }

    /// Compute the account's full snapshot as of `as_of`: every open position priced and
    /// valued, every wallet cash balance, and a grand total converted into the account's
    /// currency. See [`Self::resolve_price`] for the `provider` semantics.
    pub async fn snapshot(
        &self,
        provider: Option<&dyn StockPriceProvider>,
        account_id: i64,
        as_of: NaiveDate,
    ) -> AppResult<BrokerageSnapshot> {
        let account = self.accounts.get(account_id).await?;
        let account_ccy = account.currency_code;
        let as_of_str = as_of.to_string();
        let fx = Fx::load(self.fx.as_ref(), account_ccy.clone()).await?;

        let mut total_major = 0.0f64;

        let cost_by_ticker =
            cost_basis_by_ticker(&self.brokerage.lots_at(account_id, &as_of_str).await?, &fx);

        let mut positions = Vec::new();
        for p in self.brokerage.positions_at(account_id, &as_of_str).await? {
            let price = self
                .resolve_price(provider, &p.ticker, &p.exchange, as_of)
                .await?;
            let (price_text, price_as_of, value_minor) = match &price {
                Some(sp) => {
                    let value = market_value_minor(p.quantity, &sp.close, fx.dp(&sp.currency_code));
                    (Some(sp.close.clone()), Some(sp.as_of.clone()), value)
                }
                None => (None, None, None),
            };
            if let Some(v) = value_minor {
                // A position's price may be quoted in a different currency than the
                // ticker's listing (rare), so convert from the price's currency where we
                // have it.
                let value_ccy = price
                    .as_ref()
                    .map(|sp| sp.currency_code.as_str())
                    .unwrap_or(&p.currency_code);
                // No rate reaching the account's currency: the position keeps its own-currency
                // market value in the response, but stays out of the total, and `unconverted`
                // says so. `revalue` then refuses to persist that total at all.
                if let Some(base_major) = fx.try_to_base_major(v, value_ccy) {
                    total_major += base_major;
                }
            }
            let cost_basis_minor = cost_by_ticker
                .get(&(p.ticker.clone(), p.exchange.clone()))
                .copied()
                .flatten();
            let return_pct = match (value_minor, cost_basis_minor) {
                (Some(v), Some(c)) if c != 0 => Some((v - c) as f64 / c as f64 * 100.0),
                _ => None,
            };
            positions.push(Position {
                ticker: p.ticker,
                exchange: p.exchange,
                name: p.name,
                currency_code: p.currency_code,
                quantity: p.quantity,
                price: price_text,
                price_as_of,
                market_value_minor: value_minor,
                cost_basis_minor,
                return_pct,
            });
        }

        let mut wallets = Vec::new();
        for w in self
            .brokerage
            .wallet_balances_at(account_id, &as_of_str)
            .await?
        {
            if let Some(base_major) = fx.try_to_base_major(w.amount_minor, &w.currency_code) {
                total_major += base_major;
            }
            wallets.push(WalletBalance {
                currency_code: w.currency_code,
                amount_minor: w.amount_minor,
            });
        }

        let a = self.brokerage.activity_30d(account_id, &as_of_str).await?;

        Ok(BrokerageSnapshot {
            account_id,
            as_of: as_of_str,
            currency_code: account_ccy,
            positions,
            wallets,
            total_value_minor: fx.base_minor(total_major),
            unconverted: fx.unconverted(),
            rates_as_of: fx.rates_as_of().map(str::to_string),
            activity_30d: BrokerageActivity30d {
                contributions_minor: a.contributions_minor,
                withdrawals_minor: a.withdrawals_minor,
                trades: a.trades,
            },
        })
    }

    /// Snapshot the account's value as of `as_of` and persist it as a `source='brokerage'`
    /// valuation (upserting the day in place), so it flows into net worth. Mirrors
    /// `equity::revalue`.
    ///
    /// Refuses outright when any holding or wallet balance could not be converted into the
    /// account's currency. A snapshot may be shown partial — the response says which
    /// currencies it left out — but a *stored* valuation cannot: once it is a row in
    /// `valuations` it is indistinguishable from a complete figure, feeds net worth, and
    /// nothing downstream can tell it understated the account. That is precisely how 2,325
    /// parity-converted valuations came to exist. A day left unvalued is recoverable by
    /// re-running once a rate exists.
    pub async fn revalue(
        &self,
        provider: Option<&dyn StockPriceProvider>,
        account_id: i64,
        as_of: NaiveDate,
    ) -> AppResult<BrokerageSnapshot> {
        let snap = self.snapshot(provider, account_id, as_of).await?;
        if !snap.unconverted.is_empty() {
            return Err(AppError::validation(format!(
                "account {account_id}: no exchange rate between {} and {} as of {} — refusing \
                 to persist a valuation that would silently omit it",
                snap.unconverted.join(", "),
                snap.currency_code,
                snap.as_of,
            )));
        }
        self.valuations
            .upsert_from_brokerage(
                account_id,
                &snap.as_of,
                snap.total_value_minor,
                &snap.currency_code,
            )
            .await?;
        Ok(snap)
    }

    /// Reconstruct the account's whole net-worth history: bulk-fetch each held ticker's
    /// full daily price series in one upstream call, then walk every calendar day from
    /// the account's first activity to today, upserting a `source='brokerage'` valuation
    /// per day from the (now warm) cache. Idempotent — safe to re-run as a retry. Returns
    /// the number of days valued.
    ///
    /// A currency with no exchange rate stops the walk at the first day, by [`Self::revalue`]'s
    /// refusal: every day would be understated identically, so writing 3,000 of them and
    /// reporting success is the worst available outcome. Add the rate and re-run.
    pub async fn backfill_history(
        &self,
        provider: &dyn StockPriceProvider,
        account_id: i64,
    ) -> AppResult<usize> {
        let Some(earliest) = self.brokerage.earliest_activity_date(account_id).await? else {
            return Ok(0); // nothing imported yet
        };
        let Some(from) = NaiveDate::parse_from_str(&earliest, "%Y-%m-%d").ok() else {
            return Err(AppError::validation(
                "could not parse earliest activity date",
            ));
        };
        let today = self.clock.today();

        // One upstream call per ticker covering the full window, written through to the cache.
        for (ticker, exchange) in self.brokerage.account_tickers(account_id).await? {
            let exchange_hint = Some(exchange.as_str()).filter(|e| !e.is_empty());
            match provider
                .fetch_daily_prices(&ticker, exchange_hint, from, today)
                .await
            {
                Ok(quotes) => {
                    for q in &quotes {
                        // Exact decimal text, matching `stock_prices::price_at`'s own write.
                        self.prices
                            .upsert(
                                &ticker,
                                &exchange,
                                &q.as_of.to_string(),
                                &q.close.to_string(),
                                &q.currency_code,
                            )
                            .await?;
                    }
                }
                Err(err) => {
                    // One bad/delisted ticker shouldn't sink the whole backfill — its
                    // position simply goes unpriced for the affected days.
                    tracing::warn!(ticker = %ticker, exchange = %exchange, error = %err, "brokerage backfill: price fetch failed");
                }
            }
        }

        // Day-by-day valuation from the warm cache (provider=None → no further network calls).
        let mut day = from;
        let mut valued = 0usize;
        while day <= today {
            self.revalue(None, account_id, day).await?;
            day += chrono::Duration::days(1);
            valued += 1;
        }
        tracing::info!(account_id, days = valued, "brokerage history backfilled");
        Ok(valued)
    }

    /// The raw holdings ledger (every buy/sell/corporate lot) for an audit view.
    pub async fn list_holdings(&self, account_id: i64) -> AppResult<Vec<sure_core::HoldingLot>> {
        self.brokerage.list_holdings(account_id).await
    }

    /// Manually record a lot (most arrive via import).
    pub async fn create_holding(
        &self,
        account_id: i64,
        input: sure_core::SaveHoldingLot,
    ) -> AppResult<sure_core::HoldingLot> {
        self.brokerage.create_holding(account_id, input).await
    }

    pub async fn delete_holding(&self, id: i64) -> AppResult<()> {
        self.brokerage.delete_holding(id).await
    }

    /// Dividend/distribution history with per-jurisdiction withholding detail.
    pub async fn list_dividends(
        &self,
        account_id: i64,
    ) -> AppResult<Vec<sure_core::DividendDetail>> {
        self.brokerage.list_dividends(account_id).await
    }

    /// Persist a parsed bulk export (wallet transactions, holding lots, dividends).
    #[allow(clippy::too_many_arguments)]
    pub async fn import_export(
        &self,
        account_id: i64,
        account_currency: &str,
        provider_tag: &str,
        wallet_rows: &[crate::ports::ImportRow],
        holdings: &[crate::ports::HoldingImport],
        dividends: &[crate::ports::DividendImport],
    ) -> AppResult<crate::ports::ImportCounts> {
        self.brokerage
            .import_export(
                account_id,
                account_currency,
                provider_tag,
                wallet_rows,
                holdings,
                dividends,
            )
            .await
    }
}

/// Value in minor units of holding `quantity` units at `close` (decimal text), in the
/// price's own currency (`dp` decimal places). `None` if `close` doesn't parse.
fn market_value_minor(quantity: f64, close: &str, dp: i32) -> Option<i64> {
    let close = close.parse::<f64>().ok()?;
    Some((quantity * close * 10f64.powi(dp)).round() as i64)
}

/// Remaining cost basis per `(ticker, exchange)` in minor units of each lot's own
/// currency, average-cost method, walking lots in trade-date order (the caller fetches
/// them pre-sorted — see `lots_at`). `None` for a ticker means either it was fully exited
/// (no remaining position) or no lot ever carried a price (nothing to base a cost on) —
/// the caller treats both as "no return%".
///
/// - [`LotKind::Sell`] reduces cost proportionally to the fraction of the current
///   position it exits (average-cost realizes the rest as gain/loss, which this doesn't
///   track — only the remaining unrealized basis matters for `return_pct`).
/// - [`LotKind::Buy`] and [`LotKind::Corporate`] are handled identically, keyed off
///   whether the row has a price rather than off `kind` itself: a buy (almost) always
///   carries one, and a `corporate` row does too when it's something like a DRIP dividend
///   reinvestment — in both cases real money bought real shares, so it adds
///   `quantity × unit_price` (scaled to minor units via `fx.dp`) plus `fee_minor` to cost,
///   and `quantity` to the running total. An unpriced row (a `corporate` stock
///   split/bonus issue — or, in principle, a `buy` with no recorded price) is a
///   quantity-only adjustment instead: total cost held doesn't change, just how many
///   shares it's spread across.
fn cost_basis_by_ticker(lots: &[CostLotRow], fx: &Fx) -> HashMap<(String, String), Option<i64>> {
    struct Running {
        qty: f64,
        cost_minor: f64,
        ever_priced: bool,
    }
    let mut running: HashMap<(String, String), Running> = HashMap::new();

    for lot in lots {
        let key = (lot.ticker.clone(), lot.exchange.clone());
        let r = running.entry(key).or_insert(Running {
            qty: 0.0,
            cost_minor: 0.0,
            ever_priced: false,
        });
        match lot.kind {
            LotKind::Sell => {
                if r.qty > 0.0 {
                    let frac_sold = (-lot.quantity / r.qty).clamp(0.0, 1.0);
                    r.cost_minor -= r.cost_minor * frac_sold;
                }
                r.qty += lot.quantity;
            }
            LotKind::Buy | LotKind::Corporate => match lot.unit_price {
                Some(price) => {
                    let dp = fx.dp(&lot.currency_code);
                    r.cost_minor += lot.quantity * price * 10f64.powi(dp) + lot.fee_minor as f64;
                    r.qty += lot.quantity;
                    r.ever_priced = true;
                }
                None => {
                    r.qty += lot.quantity;
                }
            },
        }
    }

    running
        .into_iter()
        .map(|(key, r)| {
            let basis =
                (r.qty.abs() > 0.0000001 && r.ever_priced).then_some(r.cost_minor.round() as i64);
            (key, basis)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use sure_core::Ownership;

    use crate::ports::StockPriceQuote;
    use async_trait::async_trait;
    use rust_decimal::Decimal;
    use sure_core::{
        Account, AccountClass, AccountKind, AccountMetadata, BrokerageMeta, NewValuation,
        SaveAccount, Valuation,
    };

    use super::*;
    use crate::ports::{
        CostLotRow, CurrencyDecimals, ExchangeRateRow, HoldingRow, SharesTicker, WalletRow,
    };
    use crate::test_clock::FixedClock;

    fn brokerage_account(id: i64, ccy: &str) -> Account {
        Account {
            id,
            name: "Sharesies".to_string(),
            kind: AccountKind::Brokerage,
            class: AccountClass::Investment,
            currency_code: ccy.to_string(),
            institution: Some("Sharesies".to_string()),
            metadata: AccountMetadata::Brokerage(BrokerageMeta::default()),
            archived: false,
            excluded_from_net_worth: false,
            sort_order: 0,
            secured_by_account_id: None,
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            updated_at: "2026-01-01T00:00:00.000Z".to_string(),
            ownership: Ownership::Joint,
        }
    }

    struct FakeAccounts {
        account: Account,
    }
    #[async_trait]
    impl AccountRepo for FakeAccounts {
        async fn list(&self, _include_archived: bool) -> AppResult<Vec<Account>> {
            unreachable!("BrokerageService never lists accounts")
        }
        async fn get(&self, _id: i64) -> AppResult<Account> {
            Ok(self.account.clone())
        }
        async fn create(&self, _input: SaveAccount) -> AppResult<Account> {
            unreachable!("BrokerageService never creates an account")
        }
        async fn update(&self, _id: i64, _input: SaveAccount) -> AppResult<Account> {
            unreachable!("BrokerageService never updates an account")
        }
        async fn delete(&self, _id: i64) -> AppResult<()> {
            unreachable!("BrokerageService never deletes an account")
        }
        async fn set_secured_by(&self, _id: i64, _target: Option<i64>) -> AppResult<Account> {
            unreachable!("BrokerageService never mutates account metadata")
        }
        async fn set_excluded_from_net_worth(&self, _id: i64, _x: bool) -> AppResult<Account> {
            unreachable!("BrokerageService never changes net-worth inclusion")
        }
        async fn set_ownership(&self, _id: i64, _ownership: Ownership) -> AppResult<Account> {
            unreachable!("BrokerageService never attributes accounts")
        }
        async fn set_ownership_bulk(&self, _ids: &[i64], _ownership: Ownership) -> AppResult<u64> {
            unreachable!("BrokerageService never attributes accounts")
        }
        async fn list_shares_tickers(&self) -> AppResult<Vec<SharesTicker>> {
            unreachable!("BrokerageService never lists global tickers")
        }
        async fn list_brokerage_tickers(&self) -> AppResult<Vec<SharesTicker>> {
            unreachable!("BrokerageService never lists global tickers")
        }
        async fn list_house_pricer_subscriptions(
            &self,
        ) -> AppResult<Vec<crate::ports::HousePricerSubscription>> {
            unreachable!("BrokerageService never lists property-estimate subscriptions")
        }
        async fn set_house_pricer_link(
            &self,
            _account_id: i64,
            _link: Option<sure_core::HousePricerLink>,
        ) -> AppResult<Account> {
            unreachable!("BrokerageService never subscribes an account to an estimate feed")
        }
        async fn set_credit_limit(
            &self,
            _account_id: i64,
            _credit_limit_minor: i64,
        ) -> AppResult<()> {
            unreachable!("BrokerageService never mutates account metadata")
        }
        async fn set_original_amount(
            &self,
            _account_id: i64,
            _original_amount_minor: i64,
        ) -> AppResult<()> {
            unreachable!("BrokerageService never mutates account metadata")
        }
        async fn set_institution_if_unset(
            &self,
            _account_id: i64,
            _institution: &str,
        ) -> AppResult<()> {
            unreachable!("BrokerageService never mutates account metadata")
        }
        async fn set_account_number_if_unset(
            &self,
            _account_id: i64,
            _account_number: &str,
        ) -> AppResult<()> {
            unreachable!("BrokerageService never mutates account metadata")
        }
    }

    #[derive(Default, Clone)]
    struct FakeBrokerage {
        positions: Vec<HoldingRow>,
        wallets: Vec<WalletRow>,
        tickers: Vec<(String, String)>,
        earliest: Option<String>,
        lots: Vec<CostLotRow>,
        activity: crate::ports::Activity30dRow,
    }
    #[async_trait]
    impl BrokerageRepo for FakeBrokerage {
        async fn positions_at(&self, _account_id: i64, _as_of: &str) -> AppResult<Vec<HoldingRow>> {
            Ok(self.positions.clone())
        }
        async fn wallet_balances_at(
            &self,
            _account_id: i64,
            _as_of: &str,
        ) -> AppResult<Vec<WalletRow>> {
            Ok(self.wallets.clone())
        }
        async fn lots_at(&self, _account_id: i64, _as_of: &str) -> AppResult<Vec<CostLotRow>> {
            Ok(self.lots.clone())
        }
        async fn activity_30d(
            &self,
            _account_id: i64,
            _as_of: &str,
        ) -> AppResult<crate::ports::Activity30dRow> {
            Ok(self.activity.clone())
        }
        async fn account_tickers(&self, _account_id: i64) -> AppResult<Vec<(String, String)>> {
            Ok(self.tickers.clone())
        }
        async fn earliest_activity_date(&self, _account_id: i64) -> AppResult<Option<String>> {
            Ok(self.earliest.clone())
        }
        async fn list_holdings(&self, _account_id: i64) -> AppResult<Vec<sure_core::HoldingLot>> {
            unreachable!("BrokerageService never lists the raw holdings ledger")
        }
        async fn create_holding(
            &self,
            _account_id: i64,
            _input: sure_core::SaveHoldingLot,
        ) -> AppResult<sure_core::HoldingLot> {
            unreachable!("BrokerageService never creates a holding lot")
        }
        async fn delete_holding(&self, _id: i64) -> AppResult<()> {
            unreachable!("BrokerageService never deletes a holding lot")
        }
        async fn list_dividends(
            &self,
            _account_id: i64,
        ) -> AppResult<Vec<sure_core::DividendDetail>> {
            unreachable!("BrokerageService never lists dividends")
        }
        async fn import_export(
            &self,
            _account_id: i64,
            _account_currency: &str,
            _provider_tag: &str,
            _wallet_rows: &[crate::ports::ImportRow],
            _holdings: &[crate::ports::HoldingImport],
            _dividends: &[crate::ports::DividendImport],
        ) -> AppResult<crate::ports::ImportCounts> {
            unreachable!("BrokerageService never imports a bulk export")
        }
        async fn delete_holdings_by_provider(
            &self,
            _account_id: i64,
            _provider_tag: &str,
        ) -> AppResult<i64> {
            unreachable!("BrokerageService never undoes an import")
        }
        async fn delete_dividends_by_provider(
            &self,
            _account_id: i64,
            _provider_tag: &str,
        ) -> AppResult<i64> {
            unreachable!("BrokerageService never undoes an import")
        }
    }

    #[derive(Default)]
    struct FakePrices {
        rows: Mutex<HashMap<(String, String, String), StockPrice>>,
    }
    #[async_trait]
    impl StockPriceCacheRepo for FakePrices {
        async fn get_at(
            &self,
            ticker: &str,
            exchange: &str,
            as_of: &str,
        ) -> AppResult<Option<StockPrice>> {
            // Mirrors the real query: the closest cached close on or before `as_of`, not
            // an exact-date match — a weekend/holiday date still resolves to something.
            Ok(self
                .rows
                .lock()
                .unwrap()
                .values()
                .filter(|sp| {
                    sp.ticker == ticker && sp.exchange == exchange && sp.as_of.as_str() <= as_of
                })
                .max_by(|a, b| a.as_of.cmp(&b.as_of))
                .cloned())
        }
        async fn upsert(
            &self,
            ticker: &str,
            exchange: &str,
            as_of: &str,
            close: &str,
            ccy: &str,
        ) -> AppResult<()> {
            self.rows.lock().unwrap().insert(
                (ticker.to_string(), exchange.to_string(), as_of.to_string()),
                StockPrice {
                    ticker: ticker.to_string(),
                    exchange: exchange.to_string(),
                    as_of: as_of.to_string(),
                    close: close.to_string(),
                    currency_code: ccy.to_string(),
                    fetched_at: "2026-01-01T00:00:00.000Z".to_string(),
                },
            );
            Ok(())
        }
    }

    /// Mirrors the real table's `ON CONFLICT(account_id, as_of) ... DO UPDATE` — a second
    /// upsert for the same day replaces rather than accumulates, so tests can assert
    /// idempotency the same way the real backend does.
    #[derive(Default)]
    struct FakeValuations {
        rows: Mutex<HashMap<(i64, String), (i64, String)>>,
    }
    #[async_trait]
    impl ValuationRepo for FakeValuations {
        async fn list_for_account(
            &self,
            _account_id: i64,
            _q: sure_core::ValuationQuery,
        ) -> AppResult<Vec<Valuation>> {
            unreachable!("BrokerageService never lists valuations")
        }
        async fn create(&self, _account_id: i64, _input: NewValuation) -> AppResult<Valuation> {
            unreachable!("BrokerageService never creates a manual valuation")
        }
        async fn update(&self, _id: i64, _input: NewValuation) -> AppResult<Valuation> {
            unreachable!("BrokerageService never edits a valuation")
        }
        async fn delete(&self, _id: i64) -> AppResult<()> {
            unreachable!("BrokerageService never deletes a valuation")
        }
        async fn upsert_from_brokerage(
            &self,
            account_id: i64,
            as_of: &str,
            value_minor: i64,
            ccy: &str,
        ) -> AppResult<()> {
            self.rows.lock().unwrap().insert(
                (account_id, as_of.to_string()),
                (value_minor, ccy.to_string()),
            );
            Ok(())
        }
        async fn upsert_from_provider(
            &self,
            _account_id: i64,
            _as_of: &str,
            _value_minor: i64,
            _ccy: &str,
        ) -> AppResult<()> {
            unreachable!("BrokerageService never records a provider valuation")
        }
        async fn upsert_from_estimate(
            &self,
            _account_id: i64,
            _as_of: &str,
            _value_minor: i64,
            _ccy: &str,
            _note: &str,
        ) -> AppResult<()> {
            unreachable!("BrokerageService never records a property estimate")
        }
    }

    struct FakeFx {
        decimals: Vec<CurrencyDecimals>,
        rates: Vec<ExchangeRateRow>,
    }
    #[async_trait]
    impl FxRatesRepo for FakeFx {
        async fn currency_decimals(&self) -> AppResult<Vec<CurrencyDecimals>> {
            Ok(self.decimals.clone())
        }
        async fn exchange_rates(&self) -> AppResult<Vec<ExchangeRateRow>> {
            Ok(self.rates.clone())
        }
    }

    struct FakeProvider {
        close: Decimal,
        currency: String,
    }
    #[async_trait]
    impl StockPriceProvider for FakeProvider {
        fn kind(&self) -> &'static str {
            "fake"
        }
        fn description(&self) -> &'static str {
            "fake"
        }
        async fn fetch_daily_prices(
            &self,
            _ticker: &str,
            _exchange: Option<&str>,
            from: NaiveDate,
            _to: NaiveDate,
        ) -> anyhow::Result<Vec<StockPriceQuote>> {
            Ok(vec![StockPriceQuote {
                as_of: from,
                close: self.close,
                currency_code: self.currency.clone(),
            }])
        }
    }

    fn service(
        account: Account,
        brokerage: FakeBrokerage,
        clock: NaiveDate,
    ) -> (BrokerageService, Arc<FakePrices>, Arc<FakeValuations>) {
        service_with_fx(
            account,
            brokerage,
            clock,
            FakeFx {
                decimals: vec![CurrencyDecimals {
                    code: "NZD".to_string(),
                    decimal_places: 2,
                }],
                rates: vec![],
            },
        )
    }

    fn service_with_fx(
        account: Account,
        brokerage: FakeBrokerage,
        clock: NaiveDate,
        fx: FakeFx,
    ) -> (BrokerageService, Arc<FakePrices>, Arc<FakeValuations>) {
        let prices = Arc::new(FakePrices::default());
        let valuations = Arc::new(FakeValuations::default());
        let svc = BrokerageService::new(
            Arc::new(FakeAccounts { account }),
            Arc::new(brokerage),
            prices.clone(),
            valuations.clone(),
            Arc::new(fx),
            Arc::new(FixedClock(clock)),
        );
        (svc, prices, valuations)
    }

    /// A US-priced holding in an NZD account, seeded with the currencies but no rate between
    /// them. The snapshot may be partial and say so; the *valuation* must not exist at all.
    /// A stored `source='brokerage'` row is indistinguishable from a complete one, feeds net
    /// worth, and is how 2,325 parity-converted valuations came to be on the live database.
    #[tokio::test]
    async fn revalue_refuses_rather_than_persisting_an_unconverted_total() {
        let as_of = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        let (svc, _prices, valuations) = service_with_fx(
            brokerage_account(1, "NZD"),
            FakeBrokerage {
                positions: vec![HoldingRow {
                    ticker: "VOO".to_string(),
                    exchange: "NYSE".to_string(),
                    currency_code: "USD".to_string(),
                    name: Some("Vanguard S&P 500".to_string()),
                    quantity: 100.0,
                }],
                ..Default::default()
            },
            as_of,
            FakeFx {
                decimals: vec![
                    CurrencyDecimals {
                        code: "NZD".to_string(),
                        decimal_places: 2,
                    },
                    CurrencyDecimals {
                        code: "USD".to_string(),
                        decimal_places: 2,
                    },
                ],
                rates: vec![], // the whole point: no NZD/USD rate on record
            },
        );
        let provider = FakeProvider {
            close: "5.60".parse().unwrap(),
            currency: "USD".to_string(),
        };

        let snap = svc.snapshot(Some(&provider), 1, as_of).await.unwrap();
        // The position keeps its own-currency market value — that figure is true.
        assert_eq!(snap.positions[0].market_value_minor, Some(56_000));
        // …but nothing convertible went into the total, and the response says which currency.
        assert_eq!(snap.total_value_minor, 0);
        assert_eq!(snap.unconverted, vec!["USD".to_string()]);

        let err = svc
            .revalue(Some(&provider), 1, as_of)
            .await
            .expect_err("an unconverted total must not become a valuation");
        assert!(err.to_string().contains("USD"), "names the currency: {err}");
        assert!(valuations.rows.lock().unwrap().is_empty());
    }

    /// The same account once a rate exists: converted, persisted, and nothing withheld.
    /// 100 × US$5.60 = US$560 at 1 NZD = 0.6 USD => NZ$933.33.
    #[tokio::test]
    async fn revalue_persists_once_a_rate_exists() {
        let as_of = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        let (svc, _prices, valuations) = service_with_fx(
            brokerage_account(1, "NZD"),
            FakeBrokerage {
                positions: vec![HoldingRow {
                    ticker: "VOO".to_string(),
                    exchange: "NYSE".to_string(),
                    currency_code: "USD".to_string(),
                    name: Some("Vanguard S&P 500".to_string()),
                    quantity: 100.0,
                }],
                ..Default::default()
            },
            as_of,
            FakeFx {
                decimals: vec![
                    CurrencyDecimals {
                        code: "NZD".to_string(),
                        decimal_places: 2,
                    },
                    CurrencyDecimals {
                        code: "USD".to_string(),
                        decimal_places: 2,
                    },
                ],
                rates: vec![ExchangeRateRow {
                    base_code: "NZD".to_string(),
                    quote_code: "USD".to_string(),
                    rate: "0.6".to_string(),
                    as_of: "2026-01-09".to_string(),
                }],
            },
        );
        let provider = FakeProvider {
            close: "5.60".parse().unwrap(),
            currency: "USD".to_string(),
        };

        let snap = svc.revalue(Some(&provider), 1, as_of).await.unwrap();
        assert!(snap.unconverted.is_empty());
        assert_eq!(snap.total_value_minor, 93_333);
        // The rate's own date rides along, so a year-old rate is visible as one.
        assert_eq!(snap.rates_as_of.as_deref(), Some("2026-01-09"));
        assert_eq!(
            valuations.rows.lock().unwrap().get(&(1, as_of.to_string())),
            Some(&(93_333, "NZD".to_string()))
        );
    }

    #[tokio::test]
    async fn snapshot_values_positions_and_wallet_cash() {
        let as_of = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        let (svc, _prices, _valuations) = service(
            brokerage_account(1, "NZD"),
            FakeBrokerage {
                positions: vec![HoldingRow {
                    ticker: "MEL".to_string(),
                    exchange: "NZX".to_string(),
                    currency_code: "NZD".to_string(),
                    name: Some("MEL Ltd".to_string()),
                    quantity: 100.0,
                }],
                wallets: vec![WalletRow {
                    currency_code: "NZD".to_string(),
                    amount_minor: 25_00,
                }],
                ..Default::default()
            },
            as_of,
        );
        let provider = FakeProvider {
            close: "5.60".parse().unwrap(),
            currency: "NZD".to_string(),
        };

        let snap = svc.snapshot(Some(&provider), 1, as_of).await.unwrap();

        assert_eq!(snap.positions.len(), 1);
        assert_eq!(snap.positions[0].ticker, "MEL");
        // 100 shares × $5.60 = $560.00
        assert_eq!(snap.positions[0].market_value_minor, Some(56_000));
        assert_eq!(snap.wallets.len(), 1);
        assert_eq!(snap.wallets[0].amount_minor, 25_00);
        // $560 holdings + $25 wallet cash
        assert_eq!(snap.total_value_minor, 58_500);
    }

    #[tokio::test]
    async fn backfill_writes_a_valuation_per_day_and_is_idempotent() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 20).unwrap();
        let (svc, _prices, valuations) = service(
            brokerage_account(1, "NZD"),
            FakeBrokerage {
                positions: vec![HoldingRow {
                    ticker: "MEL".to_string(),
                    exchange: "NZX".to_string(),
                    currency_code: "NZD".to_string(),
                    name: Some("MEL Ltd".to_string()),
                    quantity: 10.0,
                }],
                wallets: vec![],
                tickers: vec![("MEL".to_string(), "NZX".to_string())],
                earliest: Some(today.to_string()),
                ..Default::default()
            },
            today,
        );
        let provider = FakeProvider {
            close: "2.00".parse().unwrap(),
            currency: "NZD".to_string(),
        };

        let days = svc.backfill_history(&provider, 1).await.unwrap();
        assert_eq!(days, 1); // earliest == today
        assert_eq!(valuations.rows.lock().unwrap().len(), 1);
        assert_eq!(
            valuations.rows.lock().unwrap().get(&(1, today.to_string())),
            Some(&(20_00, "NZD".to_string())) // 10 × $2.00
        );

        // Re-running upserts the same day rather than accumulating rows.
        svc.backfill_history(&provider, 1).await.unwrap();
        assert_eq!(valuations.rows.lock().unwrap().len(), 1);
    }

    fn lot(
        ticker: &str,
        qty: f64,
        price: Option<f64>,
        fee_minor: i64,
        kind: LotKind,
    ) -> CostLotRow {
        CostLotRow {
            ticker: ticker.to_string(),
            exchange: "NZX".to_string(),
            currency_code: "NZD".to_string(),
            quantity: qty,
            unit_price: price,
            fee_minor,
            kind,
        }
    }

    async fn nzd_fx() -> Fx {
        Fx::load(
            &FakeFx {
                decimals: vec![CurrencyDecimals {
                    code: "NZD".to_string(),
                    decimal_places: 2,
                }],
                rates: vec![],
            },
            "NZD".to_string(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn cost_basis_averages_and_reduces_proportionally_on_a_partial_sell() {
        let fx = nzd_fx().await;
        // Buy 10 @ $5.00 + $2 fee => $52.00 cost. Selling 4 (40% of the position) removes
        // 40% of that cost, leaving the other 6 shares at the same average per-share cost.
        let lots = vec![
            lot("MEL", 10.0, Some(5.00), 200, LotKind::Buy),
            lot("MEL", -4.0, None, 0, LotKind::Sell),
        ];
        let basis = cost_basis_by_ticker(&lots, &fx);
        assert_eq!(basis[&("MEL".to_string(), "NZX".to_string())], Some(3120));
    }

    #[tokio::test]
    async fn cost_basis_is_none_once_fully_exited() {
        let fx = nzd_fx().await;
        let lots = vec![
            lot("MEL", 10.0, Some(5.00), 200, LotKind::Buy),
            lot("MEL", -4.0, None, 0, LotKind::Sell),
            lot("MEL", -6.0, None, 0, LotKind::Sell),
        ];
        let basis = cost_basis_by_ticker(&lots, &fx);
        assert_eq!(basis[&("MEL".to_string(), "NZX".to_string())], None);
    }

    #[tokio::test]
    async fn an_unpriced_corporate_action_dilutes_per_share_cost_not_total_basis() {
        let fx = nzd_fx().await;
        // A 2-for-1 split (or bonus issue): total cost held doesn't change, only how many
        // shares it's spread across.
        let lots = vec![
            lot("FPH", 10.0, Some(5.00), 0, LotKind::Buy),
            lot("FPH", 10.0, None, 0, LotKind::Corporate),
        ];
        let basis = cost_basis_by_ticker(&lots, &fx);
        assert_eq!(basis[&("FPH".to_string(), "NZX".to_string())], Some(5000));
    }

    #[tokio::test]
    async fn a_priced_corporate_action_is_treated_like_a_buy() {
        let fx = nzd_fx().await;
        // e.g. a dividend reinvestment (DRIP): a real per-share price, adds to cost.
        let lots = vec![
            lot("AIA", 10.0, Some(5.00), 0, LotKind::Buy),
            lot("AIA", 2.0, Some(5.10), 0, LotKind::Corporate),
        ];
        let basis = cost_basis_by_ticker(&lots, &fx);
        assert_eq!(
            basis[&("AIA".to_string(), "NZX".to_string())],
            Some(5000 + 1020)
        );
    }

    #[tokio::test]
    async fn cost_basis_is_none_for_a_position_with_no_priced_lot() {
        let fx = nzd_fx().await;
        // Only an unpriced corporate action ever touched this ticker (e.g. a spin-off share
        // received with no recorded price) — nothing to base a cost on.
        let lots = vec![lot("SPUN", 5.0, None, 0, LotKind::Corporate)];
        let basis = cost_basis_by_ticker(&lots, &fx);
        assert_eq!(basis[&("SPUN".to_string(), "NZX".to_string())], None);
    }
}
