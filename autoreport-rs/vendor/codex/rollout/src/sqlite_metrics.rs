use std::sync::Arc;
use std::time::Duration;

use autoreport_otel::ORIGINATOR_TAG;
use autoreport_otel::bounded_originator_tag_value;
use autoreport_state::DbTelemetry;
use autoreport_state::DbTelemetryHandle;

struct OtelDbTelemetry {
    metrics: autoreport_otel::MetricsClient,
    originator: &'static str,
}

impl DbTelemetry for OtelDbTelemetry {
    fn counter(&self, name: &str, inc: i64, tags: &[(&str, &str)]) {
        let tags = with_originator(tags, self.originator);
        let _ = self.metrics.counter(name, inc, &tags);
    }

    fn record_duration(&self, name: &str, duration: Duration, tags: &[(&str, &str)]) {
        let tags = with_originator(tags, self.originator);
        let _ = self.metrics.record_duration(name, duration, &tags);
    }
}

pub(crate) fn recorder(
    metrics: autoreport_otel::MetricsClient,
    originator: &str,
) -> DbTelemetryHandle {
    Arc::new(OtelDbTelemetry {
        metrics,
        originator: bounded_originator_tag_value(originator),
    })
}

fn with_originator<'a>(
    tags: &[(&'a str, &'a str)],
    originator: &'static str,
) -> Vec<(&'a str, &'a str)> {
    let mut tags = tags.to_vec();
    tags.push((ORIGINATOR_TAG, originator));
    tags
}
