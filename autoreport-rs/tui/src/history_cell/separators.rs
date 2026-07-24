//! Turn separators and runtime-metrics labels for transcript history.
//! Ported from Codex's `history_cell/separators.rs`.

use super::HistoryCell;
use autoreport_core::types::RuntimeMetricsSummary;
use ratatui::style::Stylize;
use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

/// A visual divider between turns, optionally showing how long the assistant
/// "worked for" plus per-turn runtime metrics (tool/inference counts+duration).
///
/// Only emitted for turns that performed concrete work, so purely
/// conversational turns do not show an empty divider.
#[derive(Debug)]
pub(crate) struct FinalMessageSeparator {
    elapsed_seconds: Option<u64>,
    runtime_metrics: Option<RuntimeMetricsSummary>,
}

impl FinalMessageSeparator {
    pub(crate) fn new(
        elapsed_seconds: Option<u64>,
        runtime_metrics: Option<RuntimeMetricsSummary>,
    ) -> Self {
        Self {
            elapsed_seconds,
            runtime_metrics,
        }
    }
}

impl HistoryCell for FinalMessageSeparator {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut label_parts = Vec::new();
        if let Some(elapsed_seconds) = self
            .elapsed_seconds
            .filter(|seconds| *seconds > 60)
            .map(crate::bottom_pane::fmt_elapsed_compact)
        {
            label_parts.push(format!("Worked for {elapsed_seconds}"));
        }
        if let Some(metrics_label) = self.runtime_metrics.and_then(runtime_metrics_label) {
            label_parts.push(metrics_label);
        }

        if label_parts.is_empty() {
            return vec![Line::from("─".repeat(width as usize).dim())];
        }

        let label = format!("─ {} ─", label_parts.join(" • "));
        let label_width = UnicodeWidthStr::width(label.as_str());
        let label = if label_width > usize::from(width) {
            crate::line_truncation::truncate_line_with_ellipsis_if_overflow(
                Line::from(label),
                usize::from(width),
            )
            .to_string()
        } else {
            label
        };
        let used_width = UnicodeWidthStr::width(label.as_str());
        vec![
            Line::from(format!(
                "{label}{}",
                "─".repeat(usize::from(width).saturating_sub(used_width))
            ))
            .dim(),
        ]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let mut label_parts = Vec::new();
        if let Some(elapsed_seconds) = self
            .elapsed_seconds
            .filter(|seconds| *seconds > 60)
            .map(crate::bottom_pane::fmt_elapsed_compact)
        {
            label_parts.push(format!("Worked for {elapsed_seconds}"));
        }
        if let Some(metrics_label) = self.runtime_metrics.and_then(runtime_metrics_label) {
            label_parts.push(metrics_label);
        }
        if label_parts.is_empty() {
            Vec::new()
        } else {
            vec![Line::from(label_parts.join(" • "))]
        }
    }
}

/// Ported verbatim from Codex's `runtime_metrics_label` (only tool/api/streaming
/// categories can be non-zero in our runtime; the rest stay default).
pub(crate) fn runtime_metrics_label(summary: RuntimeMetricsSummary) -> Option<String> {
    let mut parts = Vec::new();
    if summary.tool_calls.count > 0 {
        let duration = format_duration_ms(summary.tool_calls.duration_ms);
        let calls = pluralize(summary.tool_calls.count, "call", "calls");
        parts.push(format!(
            "Local tools: {} {calls} ({duration})",
            summary.tool_calls.count
        ));
    }
    if summary.api_calls.count > 0 {
        let duration = format_duration_ms(summary.api_calls.duration_ms);
        let calls = pluralize(summary.api_calls.count, "call", "calls");
        parts.push(format!(
            "Inference: {} {calls} ({duration})",
            summary.api_calls.count
        ));
    }
    if summary.streaming_events.count > 0 {
        let duration = format_duration_ms(summary.streaming_events.duration_ms);
        let stream_label = pluralize(summary.streaming_events.count, "Stream", "Streams");
        let events = pluralize(summary.streaming_events.count, "event", "events");
        parts.push(format!(
            "{stream_label}: {} {events} ({duration})",
            summary.streaming_events.count
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" • "))
    }
}

fn format_duration_ms(duration_ms: u64) -> String {
    if duration_ms >= 1_000 {
        let seconds = duration_ms as f64 / 1_000.0;
        format!("{seconds:.1}s")
    } else {
        format!("{duration_ms}ms")
    }
}

fn pluralize(count: u64, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 { singular } else { plural }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreport_core::types::{RuntimeMetricTotals, RuntimeMetricsSummary};

    #[test]
    fn empty_summary_yields_no_label() {
        assert!(runtime_metrics_label(RuntimeMetricsSummary::default()).is_none());
    }

    #[test]
    fn label_lists_tools_and_inference() {
        let summary = RuntimeMetricsSummary {
            tool_calls: RuntimeMetricTotals {
                count: 3,
                duration_ms: 2_500,
            },
            api_calls: RuntimeMetricTotals {
                count: 2,
                duration_ms: 800,
            },
            ..Default::default()
        };
        let label = runtime_metrics_label(summary).unwrap();
        assert!(label.contains("Local tools: 3 calls (2.5s)"), "{label}");
        assert!(label.contains("Inference: 2 calls (800ms)"), "{label}");
        assert!(label.contains(" • "), "{label}");
    }

    #[test]
    fn single_call_uses_singular() {
        let summary = RuntimeMetricsSummary {
            tool_calls: RuntimeMetricTotals {
                count: 1,
                duration_ms: 500,
            },
            ..Default::default()
        };
        let label = runtime_metrics_label(summary).unwrap();
        assert!(label.contains("1 call (500ms)"), "{label}");
    }
}
