use super::{StabilityResult, lookup::observed_scope, stability::comparison};
use crate::{
    model::CheckOutcome,
    snapshot::{FileEntry, ObservationStatus, Snapshot},
};
use std::{collections::BTreeMap, path::Path};

/// Compare complete no-follow observations for a real path or subtree change.
/// Observed creation/removal establishes change; absence at both stages does not.
/// Uncertainty at either stage takes precedence over any apparent difference.
pub fn compare_changed(path: &str, before: &[Snapshot], after: &[Snapshot]) -> StabilityResult {
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
    if !exists(path, old) && !exists(path, new) {
        return comparison(
            CheckOutcome::Failed,
            format!("declared changed path is absent before and after: {path}"),
        );
    }
    if subtree(path, old) == subtree(path, new) {
        comparison(
            CheckOutcome::Failed,
            format!("declared changed scope is unchanged: {path}"),
        )
    } else {
        comparison(
            CheckOutcome::Passed,
            format!("declared changed scope differs: {path}"),
        )
    }
}

/// Compare an existing known-good subtree with its mapped backup subtree.
/// Relative component paths and all FileEntry fields must match. Only ancestor
/// scaffolding outside these scopes is ignored; symlinks are never followed.
pub fn compare_mapped(
    source: &str,
    stored: &str,
    known_good: &[Snapshot],
    backup: &[Snapshot],
) -> StabilityResult {
    let original = observed_scope(source, known_good);
    let copy = observed_scope(stored, backup);
    let (original, copy) = match (original, copy) {
        (Err(reason), _) => {
            return comparison(CheckOutcome::Incomplete, format!("source: {reason}"));
        }
        (_, Err(reason)) => {
            return comparison(CheckOutcome::Incomplete, format!("backup: {reason}"));
        }
        (Ok(original), Ok(copy)) => (original, copy),
    };
    for (stage, path, snapshot) in [("source", source, original), ("backup", stored, copy)] {
        if !exists(path, snapshot) {
            return comparison(
                CheckOutcome::Failed,
                format!("mapped {stage} path is absent: {path}"),
            );
        }
    }
    if subtree(source, original) == subtree(stored, copy) {
        comparison(
            CheckOutcome::Passed,
            format!("mapped backup matches source scope: {source} -> {stored}"),
        )
    } else {
        comparison(
            CheckOutcome::Failed,
            format!("mapped backup differs from source scope: {source} -> {stored}"),
        )
    }
}

fn exists(path: &str, snapshot: &Snapshot) -> bool {
    snapshot.completeness != ObservationStatus::Missing && snapshot.entries.contains_key(path)
}

/// Rebase by path components, so e.g. /data-old cannot enter /data's scope.
fn subtree<'a>(root: &str, snapshot: &'a Snapshot) -> BTreeMap<&'a Path, &'a FileEntry> {
    if snapshot.completeness == ObservationStatus::Missing {
        return BTreeMap::new();
    }
    snapshot
        .entries
        .iter()
        .filter_map(|(path, entry)| {
            Path::new(path)
                .strip_prefix(root)
                .ok()
                .map(|relative| (relative, entry))
        })
        .collect()
}
