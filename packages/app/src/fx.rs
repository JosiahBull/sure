//! Currency conversion using the latest known rates, shared by the report aggregation
//! (`crate::reports`) and the brokerage snapshot (`crate::brokerage`). For a
//! single-family tracker, applying current rates across history is a fine approximation
//! and keeps figures readable rather than jumping around with historical fx noise.

use std::collections::HashMap;

use sure_core::AppResult;

use crate::ports::FxRatesRepo;

pub struct Fx {
    base: String,
    /// (base_code, quote_code) => 1 base = rate quote.
    rates: HashMap<(String, String), f64>,
    decimals: HashMap<String, i32>,
}

impl Fx {
    pub async fn load(repo: &dyn FxRatesRepo, base: String) -> AppResult<Self> {
        let decimals = repo
            .currency_decimals()
            .await?
            .into_iter()
            .map(|c| (c.code, c.decimal_places))
            .collect();

        // Ordered by date so later rows overwrite earlier => the latest rate wins.
        let mut rates = HashMap::new();
        for r in repo.exchange_rates().await? {
            if let Ok(v) = r.rate.parse::<f64>() {
                rates.insert((r.base_code, r.quote_code), v);
            }
        }
        Ok(Self {
            base,
            rates,
            decimals,
        })
    }

    pub fn dp(&self, ccy: &str) -> i32 {
        self.decimals.get(ccy).copied().unwrap_or(2)
    }

    /// Multiplier converting 1 unit of `ccy` into the base currency.
    pub fn factor(&self, ccy: &str) -> f64 {
        if ccy == self.base {
            return 1.0;
        }
        if let Some(r) = self.rates.get(&(self.base.clone(), ccy.to_string())) {
            if *r != 0.0 {
                return 1.0 / r;
            }
        }
        if let Some(r) = self.rates.get(&(ccy.to_string(), self.base.clone())) {
            return *r;
        }
        1.0 // unknown pair: assume parity rather than dropping the account
    }

    /// Convert minor units of `ccy` into base-currency major units.
    pub fn to_base_major(&self, amount_minor: i64, ccy: &str) -> f64 {
        let major = amount_minor as f64 / 10f64.powi(self.dp(ccy));
        major * self.factor(ccy)
    }

    pub fn base_minor(&self, base_major: f64) -> i64 {
        (base_major * 10f64.powi(self.dp(&self.base))).round() as i64
    }
}
