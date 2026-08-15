use axum::Router;

use crate::config::Limits;
use crate::state::AppState;

pub mod accounts;
pub mod brokerage;
pub mod categories;
pub mod crons;
pub mod currencies;
pub mod equity;
pub mod forecast;
pub mod health;
pub mod import;
pub mod income;
pub mod merchants;
pub mod people;
pub mod property_estimates;
pub mod providers;
pub mod reports;
pub mod rules;
pub mod settings;
pub mod snapshot;
pub mod stock_prices;
pub mod transactions;
pub mod valuations;

/// The full API surface, mounted under `/api`.
///
/// `limits` is threaded through for the two routes that override the global request-body cap:
/// an upload to `/import` (a Sharesies export zip, a myIR spreadsheet, an ASB transaction CSV,
/// or a zip of those) and a config snapshot are both legitimately far larger than anything else
/// the API accepts.
pub fn router(limits: &Limits) -> Router<AppState> {
    let api = health::router()
        .merge(currencies::router())
        .merge(settings::router())
        .merge(categories::router())
        .merge(merchants::router())
        .merge(people::router())
        .merge(accounts::router())
        .merge(transactions::router())
        .merge(valuations::router())
        .merge(rules::router())
        .merge(crons::router())
        .merge(equity::router())
        .merge(brokerage::router())
        .merge(import::router(limits))
        .merge(providers::router())
        .merge(snapshot::router(limits))
        .merge(reports::router())
        .merge(stock_prices::router())
        .merge(property_estimates::router())
        .merge(forecast::router())
        .merge(income::router());
    Router::new().nest("/api", api)
}
