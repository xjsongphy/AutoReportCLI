//! Rollout entry projections.

use crate::{ResponseItem, RolloutEntry};

/// Items only (drops the meta header) from a rollout read.
pub fn items(entries: &[RolloutEntry]) -> Vec<ResponseItem> {
    entries
        .iter()
        .filter_map(|e| match e {
            RolloutEntry::Item(i) => Some(i.clone()),
            _ => None,
        })
        .collect()
}
