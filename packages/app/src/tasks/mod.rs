//! Scheduled-task bodies, registered with `sure_scheduler::Scheduler` by the
//! composition root (`sure-api`'s `serve()`).

pub mod balance_delta;
pub mod exchange_rates;
pub mod provider_poll;
pub mod transfer_link;
