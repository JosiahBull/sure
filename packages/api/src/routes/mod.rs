use axum::Router;

use crate::config::Limits;
use crate::state::AppState;

pub mod accounts;
pub mod asb;
pub mod brokerage;
pub mod categories;
pub mod crons;
pub mod currencies;
pub mod equity;
pub mod forecast;
pub mod health;
pub mod merchants;
pub mod people;
pub mod providers;
pub mod reports;
pub mod rules;
pub mod settings;
pub mod snapshot;
pub mod stock_prices;
pub mod student_loan;
pub mod transactions;
pub mod valuations;

/// The full API surface, mounted under `/api`.
///
/// `limits` is threaded through for the routes that override the global request-body cap —
/// a Sharesies export zip, a myIR spreadsheet, an ASB transaction CSV, and a config
/// snapshot are all legitimately far larger than anything else the API accepts.
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
        .merge(brokerage::router(limits))
        .merge(providers::router())
        .merge(snapshot::router(limits))
        .merge(reports::router())
        .merge(stock_prices::router())
        .merge(student_loan::router(limits))
        .merge(asb::router(limits))
        .merge(forecast::router());
    Router::new().nest("/api", api)
}
