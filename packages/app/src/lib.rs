// Money is stored in minor units and written with a `dollars_cents` digit grouping
// (e.g. `485_000_00` == $485,000.00), which reads far better than 3-digit groups for
// financial values; clippy's grouping lint fights it, so allow it crate-wide.
#![allow(clippy::inconsistent_digit_grouping)]

pub mod brokerage;
pub mod fx;
pub mod reports;
pub mod rules;
pub mod stock_prices;
pub mod sync;
pub mod tasks;
