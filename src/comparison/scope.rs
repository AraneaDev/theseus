//! Pure, no-follow scoped evidence comparison with bounded difference storage.
use super::{
    budget::{self, Budget},
    model::*,
};
use crate::{
    checks::lookup::{ObservationError, observed_scope_reason},
    model::FileKind,
    recipe::{normalize_container_path, validate::contained},
    snapshot::{ChangeKind, FileChange, FileEntry, ObservationStatus, Snapshot},
};
use std::collections::BTreeMap;

fn evidence<'a>(
    snapshots: &'a [Snapshot],
    path: &'a str,
) -> Result<BTreeMap<&'a str, &'a FileEntry>, ObservationError<'a>> {
    let snapshot = observed_scope_reason(path, snapshots)?;
    if snapshot.root == "/"
        || !normalize_container_path(&snapshot.root).is_ok_and(|p| p == snapshot.root)
    {
        return Err(ObservationError([
            "observation root must be a normalized absolute application path",
            "",
            "",
            "",
            "",
        ]));
    }
    if snapshot.completeness == ObservationStatus::Missing {
        return if snapshot.entries.is_empty() {
            Ok(BTreeMap::new())
        } else {
            Err(ObservationError([
                "missing observation contains entries",
                "",
                "",
                "",
                "",
            ]))
        };
    }
    if !snapshot.entries.contains_key(&snapshot.root) {
        return Err(ObservationError([
            "complete observation has no root entry: ",
            &snapshot.root,
            "",
            "",
            "",
        ]));
    }
    // Reject malformed evidence rather than allowing an impossible tree to prove absence.
    for key in snapshot.entries.keys() {
        if !normalize_container_path(key).is_ok_and(|p| p == *key)
            || !contained(key, &snapshot.root)
        {
            return Err(ObservationError([
                "invalid observation entry: ",
                key,
                "",
                "",
                "",
            ]));
        }
        if key != &snapshot.root {
            let parent = key.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
            if !snapshot
                .entries
                .get(parent)
                .is_some_and(|e| e.kind == FileKind::Directory)
            {
                return Err(ObservationError([
                    "missing or non-directory parent for ",
                    key,
                    "",
                    "",
                    "",
                ]));
            }
        }
    }
    Ok(snapshot
        .entries
        .iter()
        .filter(|(key, _)| contained(key, path))
        .map(|(key, entry)| {
            let relative = if key == path {
                "."
            } else {
                &key[path.len() + 1..]
            };
            (relative, entry)
        })
        .collect())
}
/// Compare complete subtree metadata, including verified absence, without reading disk.
pub fn compare_scope(before: &[Snapshot], after: &[Snapshot], path: &str) -> ScopeComparison {
    let mut bytes = Budget::new(budget::MAX_DERIVED_BYTES, budget::CELL_RESERVE).unwrap();
    compare_scope_bounded(before, after, path, MAX_CELL_DIFFERENCES, &mut bytes)
}
pub(crate) fn compare_scope_bounded(
    before: &[Snapshot],
    after: &[Snapshot],
    path: &str,
    budget: usize,
    bytes: &mut Budget,
) -> ScopeComparison {
    if path == "/" || !normalize_container_path(path).is_ok_and(|p| p == path) {
        return ScopeComparison::unknown("scope path must be normalized absolute application path");
    }
    let old = match evidence(before, path) {
        Ok(e) => e,
        Err(e) => return diagnostic("before: ", e, bytes),
    };
    let new = match evidence(after, path) {
        Ok(e) => e,
        Err(e) => return diagnostic("after: ", e, bytes),
    };
    let mut result = ScopeComparison {
        status: ComparisonStatus::Equivalent,
        differences: vec![],
        diagnostics: vec![],
    };
    // Merge ordered keys without allocating an unbounded union or complete diff.
    let mut old = old.into_iter().peekable();
    let mut new = new.into_iter().peekable();
    while old.peek().is_some() || new.peek().is_some() {
        let key = match (old.peek(), new.peek()) {
            (Some((a, _)), Some((b, _))) => *a.min(b),
            (Some((a, _)), None) => *a,
            (None, Some((b, _))) => *b,
            _ => unreachable!(),
        };
        let a = if old.peek().is_some_and(|(p, _)| *p == key) {
            old.next().map(|(_, e)| e)
        } else {
            None
        };
        let b = if new.peek().is_some_and(|(p, _)| *p == key) {
            new.next().map(|(_, e)| e)
        } else {
            None
        };
        if a == b {
            continue;
        }
        if result.differences.len() >= budget {
            result.status = ComparisonStatus::Unknown;
            result
                .diagnostics
                .push("difference storage limit exceeded".into());
            return result;
        }
        let cost = budget::strings(256, [key])
            .and_then(|n| n.checked_add(budget::entry(a)?))
            .and_then(|n| n.checked_add(budget::entry(b)?));
        if !bytes.admit(cost) {
            result.status = ComparisonStatus::Unknown;
            result.diagnostics.push(budget::BYTE_LIMIT.into());
            return result;
        }
        result.status = ComparisonStatus::Different;
        result.differences.push(FileChange {
            path: key.into(),
            kind: match (a, b) {
                (None, _) => ChangeKind::Added,
                (_, None) => ChangeKind::Removed,
                _ => ChangeKind::Modified,
            },
            before: a.cloned(),
            after: b.cloned(),
        });
    }
    result
}

fn diagnostic(prefix: &str, error: ObservationError<'_>, bytes: &mut Budget) -> ScopeComparison {
    if bytes.admit(budget::strings(64, std::iter::once(prefix).chain(error.0))) {
        ScopeComparison::unknown(format!("{prefix}{}", error.0.concat()))
    } else {
        ScopeComparison::unknown(budget::BYTE_LIMIT)
    }
}
