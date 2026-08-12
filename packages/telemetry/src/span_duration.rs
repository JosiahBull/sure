//! [`DurationLayer`]: turn the time a span was open into a histogram observation.

use std::time::Instant;

use opentelemetry::KeyValue;
use tracing::span;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// The target prefix whose spans become `db.client.operation.duration`.
///
/// `sure_dal`'s repository functions each already carry
/// `#[tracing::instrument(level = "debug", skip_all)]`, and a span's name is the function's own
/// name. That gives per-query timing for the whole data layer with no call-site changes and a
/// bounded label set — one value of `db.operation.name` per repository function, not per SQL
/// string and certainly not per parameter.
const DAL_TARGET: &str = "sure_dal";

/// Recorded into a span's extensions when it opens, read when it closes.
struct OpenedAt(Instant);

/// Times spans from the data layer and records them as `db.client.operation.duration`.
///
/// # Why a layer instead of instrumenting the queries
///
/// There are 176 `#[tracing::instrument]` attributes in `sure-dal` already, and every one of
/// them names the operation. Timing them here means no call site changes, nothing to forget
/// when a repository function is added, and no second opinion about what an operation is
/// called. The alternative — a macro or a helper wrapped around every query — would be 176
/// edits that then have to be kept in step by hand.
///
/// # What it measures
///
/// Wall-clock from `on_new_span` to `on_close`: the span's whole lifetime, including time the
/// future spent parked. That is the number a caller actually waits, which is what a latency
/// histogram should say. (`tracing`'s own `time.busy`/`time.idle` split is available on the fmt
/// output for anyone who needs to know *why* an operation was slow.)
///
/// # Cardinality
///
/// One attribute, `db.operation.name`, built by [`operation_name`] from the span's module and
/// name — both `&'static str` from a `#[instrument]` attribute, so the set is fixed at compile
/// time at one value per repository function. Span *fields* are never read, which is what keeps
/// bound parameter values out of the metric labels.
#[derive(Default)]
pub struct DurationLayer;

impl DurationLayer {
    pub fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for DurationLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, _attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        if !span.metadata().target().starts_with(DAL_TARGET) {
            return;
        }
        // Only DAL spans get an extension, so a request's own spans cost nothing here.
        span.extensions_mut().insert(OpenedAt(Instant::now()));
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        // Absent for every span this layer did not open — the check is the filter.
        let Some(OpenedAt(started)) = span.extensions_mut().remove::<OpenedAt>() else {
            return;
        };
        let metadata = span.metadata();
        crate::instruments::instruments()
            .db_operation_duration
            .record(
                crate::instruments::secs(started.elapsed()),
                &[KeyValue::new(
                    "db.operation.name",
                    operation_name(metadata.target(), metadata.name()),
                )],
            );
    }
}

/// `sure_dal::accounts` + `list` becomes `accounts.list`.
///
/// The module has to be in there. A `#[tracing::instrument]` with no `name` takes the
/// function's, and `sure-dal` has a `list` in `accounts`, `currencies`, `transactions`,
/// `people`, `rules` and a dozen more — so the span name alone collapses all of them into one
/// series, which is what the first end-to-end export of this metric actually showed (values
/// `get`, `list`, `seed`, and no way to tell which table). Qualifying by module is still a
/// bounded label: one value per repository function.
///
/// Costs a small `String` per DAL span close, because a metric attribute needs an owned value
/// and the two halves only exist separately. Set against what the span it is measuring just
/// did — a SQLite query — it does not register.
fn operation_name(target: &str, name: &str) -> String {
    match target.strip_prefix(DAL_TARGET).and_then(|rest| {
        // `sure_dal::accounts` -> `accounts`; bare `sure_dal` -> nothing to qualify with.
        rest.strip_prefix("::").filter(|module| !module.is_empty())
    }) {
        Some(module) => format!("{module}.{name}"),
        None => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

    /// Captures what `DurationLayer` would have recorded, by doing the same span bookkeeping
    /// against a list instead of a histogram. The real layer records into the global meter,
    /// which in a test process is a no-op that cannot be read back — so the behaviour under
    /// test is "which spans are selected, and does a duration come out", which this mirrors.
    #[derive(Default)]
    struct Spy {
        recorded: Arc<Mutex<Vec<(String, std::time::Duration)>>>,
    }

    impl<S> Layer<S> for Spy
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(&self, _a: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
            let Some(span) = ctx.span(id) else { return };
            if !span.metadata().target().starts_with(DAL_TARGET) {
                return;
            }
            span.extensions_mut().insert(OpenedAt(Instant::now()));
        }

        fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
            let Some(span) = ctx.span(&id) else { return };
            let Some(OpenedAt(started)) = span.extensions_mut().remove::<OpenedAt>() else {
                return;
            };
            self.recorded.lock().unwrap().push((
                operation_name(span.metadata().target(), span.metadata().name()),
                started.elapsed(),
            ));
        }
    }

    fn recorded_for(body: impl FnOnce()) -> Vec<(String, std::time::Duration)> {
        let spy = Spy::default();
        let recorded = Arc::clone(&spy.recorded);
        tracing::subscriber::with_default(Registry::default().with(spy), body);
        recorded.lock().unwrap().clone()
    }

    #[test]
    fn a_dal_span_is_timed_and_named_by_its_module_and_function() {
        let recorded = recorded_for(|| {
            // The real shape: `#[tracing::instrument(level = "debug", skip_all)]` on
            // `sure_dal::accounts::list` takes the function's name and the module's target.
            let span = tracing::debug_span!(target: "sure_dal::accounts", "list");
            let entered = span.enter();
            std::thread::sleep(std::time::Duration::from_millis(5));
            drop(entered);
            drop(span);
        });

        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert_eq!(recorded[0].0, "accounts.list");
        assert!(
            recorded[0].1 >= std::time::Duration::from_millis(5),
            "the span was open for at least the sleep: {:?}",
            recorded[0].1
        );
    }

    /// The layer has to be selective, or every handler span and the request span itself would
    /// also arrive as a "database operation".
    #[test]
    fn spans_from_other_crates_are_ignored() {
        let recorded = recorded_for(|| {
            tracing::info_span!(target: "sure_api::telemetry", "http.request").in_scope(|| {});
            tracing::debug_span!(target: "sure_app::reports", "reports.net_worth").in_scope(|| {});
        });
        assert!(recorded.is_empty(), "{recorded:?}");
    }

    /// The bug the first real export of this metric revealed: `#[instrument]` with no `name`
    /// uses the function's, and `sure-dal` has a `list` in a dozen modules — so without the
    /// module every one of them lands in the same series. The observed values were `get`,
    /// `list` and `seed`, which name no table at all.
    #[test]
    fn an_operation_is_qualified_by_its_module() {
        assert_eq!(
            operation_name("sure_dal::accounts", "list"),
            "accounts.list"
        );
        assert_eq!(
            operation_name("sure_dal::tax_scales", "seed"),
            "tax_scales.seed"
        );
        // A span logged against the crate root has nothing to qualify with.
        assert_eq!(
            operation_name("sure_dal", "with_busy_retry"),
            "with_busy_retry"
        );
        // No DAL span carries an explicit `name = ..` today — every one is its function's name
        // — so a name that already contains a module would double the prefix. Left unhandled
        // deliberately: guessing at it would be untested code for a case that does not exist,
        // and `accounts.accounts.list` on a dashboard is self-explaining if it ever shows up.
    }

    /// Two functions of the same name in different modules must not share a series.
    #[test]
    fn the_same_function_name_in_two_modules_stays_two_operations() {
        let recorded = recorded_for(|| {
            tracing::debug_span!(target: "sure_dal::accounts", "list").in_scope(|| {});
            tracing::debug_span!(target: "sure_dal::currencies", "list").in_scope(|| {});
        });
        let names: Vec<_> = recorded.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["accounts.list", "currencies.list"]);
    }

    /// A span entered and exited more than once — an `async` DAL function awaited across a
    /// yield point does exactly this — must still produce one observation, on close.
    #[test]
    fn a_span_reentered_several_times_is_recorded_once() {
        let recorded = recorded_for(|| {
            let span = tracing::debug_span!(target: "sure_dal", "transactions.list");
            span.in_scope(|| {});
            span.in_scope(|| {});
            drop(span);
        });
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert_eq!(recorded[0].0, "transactions.list");
    }
}
