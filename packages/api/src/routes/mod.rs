use axum::Router;

use crate::state::AppState;

pub mod accounts;
pub mod categories;
pub mod crons;
pub mod currencies;
pub mod equity;
pub mod health;
pub mod merchants;
pub mod providers;
pub mod reports;
pub mod rules;
pub mod settings;
pub mod snapshot;
pub mod stock_prices;
pub mod transactions;
pub mod valuations;

/// The full API surface, mounted under `/api`.
pub fn router() -> Router<AppState> {
    let api = health::router()
        .merge(currencies::router())
        .merge(settings::router())
        .merge(categories::router())
        .merge(merchants::router())
        .merge(accounts::router())
        .merge(transactions::router())
        .merge(valuations::router())
        .merge(rules::router())
        .merge(crons::router())
        .merge(equity::router())
        .merge(providers::router())
        .merge(snapshot::router())
        .merge(reports::router())
        .merge(stock_prices::router());
    Router::new().nest("/api", api)
}
