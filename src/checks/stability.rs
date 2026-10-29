use super::lookup::observed_scope;
use crate::{
    model::CheckOutcome,
    recipe::validate::contained,
    snapshot::{ObservationStatus, Snapshot, diff},
};

/// Result for a declared path and its observed descendants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StabilityResult {
    pub outcome: CheckOutcome,
    pub message: String,
}

/// Compare complete no-follow observations, including metadata and child changes.
/// An absent reference object cannot establish stability. Incomplete evidence at
/// either stage takes precedence over an observed absence or difference.
pub fn compare_unchanged(path: &str, before: &[Snapshot], after: &[Snapshot]) -> StabilityResult {
    let old = observed_scope(path, before);
    let new = observed_scope(path, after);
    let (old, new) = match (old, new) {
        (Err(reason), _) => {
            return comparison(CheckOutcome::Incomplete, format!("before: {reason}"));
        }
        (_, Err(reason)) => {
            return comparison(CheckOutcome::Incomplete, format!("after: {reason}"));
        }
        (Ok(old), Ok(new)) => (old, new),
    };
    for (stage, snapshot) in [("before", old), ("after", new)] {
        if snapshot.completeness == ObservationStatus::Missing
            || !snapshot.entries.contains_key(path)
        {
            return comparison(
                CheckOutcome::Failed,
                format!("declared stable path is absent {stage}: {path}"),
            );
        }
    }
    let changed = diff(old, new)
        .iter()
        .filter(|change| contained(&change.path, path))
        .count();
    if changed == 0 {
        comparison(
            CheckOutcome::Passed,
            format!("declared stable scope is unchanged: {path}"),
        )
    } else {
        comparison(
            CheckOutcome::Failed,
            format!("{changed} changed entries in declared stable scope: {path}"),
        )
    }
}

pub(super) fn comparison(outcome: CheckOutcome, mut message: String) -> StabilityResult {
    // Detailed entries remain in stage evidence; even observer error text must
    // not turn a check summary into an unbounded report field.
    const MAX_MESSAGE_BYTES: usize = 1024;
    if message.len() > MAX_MESSAGE_BYTES {
        let mut end = MAX_MESSAGE_BYTES - "…".len();
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push('…');
    }
    StabilityResult { outcome, message }
}
