//! [`MaxLevel`]: drop events above a verbosity, forward everything else untouched.

use tracing::level_filters::LevelFilter;
use tracing::{Event, Metadata, span};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// Wraps a layer so it never *sees* an event more verbose than `max`, while every other
/// callback reaches it verbatim.
///
/// # Why this exists instead of `Layer::with_filter`
///
/// A per-layer filter (`Filtered`) is assigned its `FilterId` when the **subscriber is
/// built**. The OTLP layers cannot be present then — building an exporter spawns a thread, and
/// the Landlock sandbox has to go on while the process is still single-threaded — so they are
/// installed later through a `tracing_subscriber::reload` slot. A `Filtered` layer swapped in
/// that way panics on first use with *"a `Filtered` layer was used, but it had no `FilterId`;
/// was it registered with the subscriber?"*. There is a test for that in
/// `sure_api::telemetry`.
///
/// # Why it gates `on_event` and not `event_enabled`
///
/// `event_enabled` is the hook for a layer to veto an event for the *whole* subscriber:
/// `Layered` combines the answers, so a `false` here would also stop the fmt layer printing
/// the line. Suppressing in `on_event` keeps the decision local to the wrapped layer.
///
/// # Why it forwards every other callback
///
/// Span bookkeeping stays intact. A wrapper that skipped `on_new_span` for filtered spans
/// would leave a layer holding per-span state — which `tracing-opentelemetry` does — being
/// told about closes for spans it never opened. Here the only thing withheld is a log record.
pub struct MaxLevel<L> {
    inner: L,
    max: LevelFilter,
}

impl<L> MaxLevel<L> {
    pub fn new(inner: L, max: LevelFilter) -> Self {
        Self { inner, max }
    }

    fn permits(&self, metadata: &Metadata<'_>) -> bool {
        // `Level`'s ordering runs ERROR < WARN < INFO < DEBUG < TRACE, so "no more verbose
        // than max" is `<=`. `LevelFilter::OFF` has no `Level` and permits nothing.
        self.max
            .into_level()
            .is_some_and(|max| *metadata.level() <= max)
    }
}

impl<S, L> Layer<S> for MaxLevel<L>
where
    S: tracing::Subscriber,
    L: Layer<S>,
{
    fn on_register_dispatch(&self, subscriber: &tracing::Dispatch) {
        self.inner.on_register_dispatch(subscriber);
    }

    fn on_layer(&mut self, subscriber: &mut S) {
        self.inner.on_layer(subscriber);
    }

    fn register_callsite(
        &self,
        metadata: &'static Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        self.inner.register_callsite(metadata)
    }

    fn enabled(&self, metadata: &Metadata<'_>, ctx: Context<'_, S>) -> bool {
        self.inner.enabled(metadata, ctx)
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        self.inner.max_level_hint()
    }

    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        self.inner.on_new_span(attrs, id, ctx);
    }

    fn on_record(&self, span: &span::Id, values: &span::Record<'_>, ctx: Context<'_, S>) {
        self.inner.on_record(span, values, ctx);
    }

    fn on_follows_from(&self, span: &span::Id, follows: &span::Id, ctx: Context<'_, S>) {
        self.inner.on_follows_from(span, follows, ctx);
    }

    fn event_enabled(&self, event: &Event<'_>, ctx: Context<'_, S>) -> bool {
        // Forwarded, deliberately un-gated — see the type docs.
        self.inner.event_enabled(event, ctx)
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if self.permits(event.metadata()) {
            self.inner.on_event(event, ctx);
        }
    }

    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        self.inner.on_enter(id, ctx);
    }

    fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
        self.inner.on_exit(id, ctx);
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        self.inner.on_close(id, ctx);
    }

    fn on_id_change(&self, old: &span::Id, new: &span::Id, ctx: Context<'_, S>) {
        self.inner.on_id_change(old, new, ctx);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

    #[derive(Default)]
    struct Counts {
        events: AtomicUsize,
        spans: AtomicUsize,
        closes: AtomicUsize,
    }

    struct Recording(Arc<Counts>);

    impl<S: tracing::Subscriber> Layer<S> for Recording {
        fn on_event(&self, _event: &Event<'_>, _ctx: Context<'_, S>) {
            self.0.events.fetch_add(1, Ordering::Relaxed);
        }
        fn on_new_span(&self, _a: &span::Attributes<'_>, _i: &span::Id, _c: Context<'_, S>) {
            self.0.spans.fetch_add(1, Ordering::Relaxed);
        }
        fn on_close(&self, _id: span::Id, _ctx: Context<'_, S>) {
            self.0.closes.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn run(max: LevelFilter) -> Arc<Counts> {
        let counts = Arc::new(Counts::default());
        let layer = MaxLevel::new(Recording(Arc::clone(&counts)), max);
        tracing::subscriber::with_default(Registry::default().with(layer), || {
            tracing::info!("an info event");
            tracing::debug!("a debug event");
            tracing::debug!("another debug event");
            let span = tracing::debug_span!("a debug span");
            drop(span.enter());
            drop(span);
        });
        counts
    }

    #[test]
    fn events_above_the_maximum_are_dropped() {
        let counts = run(LevelFilter::INFO);
        assert_eq!(
            counts.events.load(Ordering::Relaxed),
            1,
            "only the info event"
        );
    }

    #[test]
    fn a_permissive_maximum_lets_everything_through() {
        let counts = run(LevelFilter::TRACE);
        assert_eq!(counts.events.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn off_drops_every_event() {
        let counts = run(LevelFilter::OFF);
        assert_eq!(counts.events.load(Ordering::Relaxed), 0);
    }

    /// The property that makes this safe to wrap a stateful layer in: spans are never hidden,
    /// however aggressively events are. A `tracing-opentelemetry` layer told about a close for
    /// a span it never opened is the failure this rules out.
    #[test]
    fn span_bookkeeping_is_forwarded_even_below_the_maximum() {
        let counts = run(LevelFilter::ERROR);
        assert_eq!(
            counts.events.load(Ordering::Relaxed),
            0,
            "no events survive"
        );
        assert_eq!(
            counts.spans.load(Ordering::Relaxed),
            1,
            "the debug span must still be opened"
        );
        assert_eq!(
            counts.closes.load(Ordering::Relaxed),
            1,
            "and closed, or a stateful inner layer leaks"
        );
    }
}
