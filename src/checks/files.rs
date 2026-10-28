use super::result;
use crate::recipe::validate::contained;
use crate::{
    model::{CheckOutcome, CheckResult, Checkpoint, FileKind, Finding, FindingKind},
    recipe::{Check, Expectation},
    snapshot::{FileEntry, ObservationStatus, Snapshot},
};
use std::collections::{BTreeMap, BTreeSet};

/// The outer Result distinguishes unobserved evidence from an observed missing path.
fn observed<'a>(path: &str, snapshots: &'a [Snapshot]) -> Result<Option<&'a FileEntry>, String> {
    let mut matching = snapshots.iter().filter(|s| contained(path, &s.root));
    let snapshot = matching
        .next()
        .ok_or_else(|| format!("no observed root covers {path}"))?;
    if matching.next().is_some() {
        return Err(format!("ambiguous overlapping observations cover {path}"));
    }
    match &snapshot.completeness {
        ObservationStatus::Complete => {
            // A no-follow snapshot proves the link entry, not anything reached
            // through it. The same uncertainty applies to non-directory parents.
            // Inspect proper ancestors only, so checking the link itself stays valid.
            for (index, _) in path.match_indices('/').skip(1) {
                let ancestor = &path[..index];
                if snapshot
                    .entries
                    .get(ancestor)
                    .is_some_and(|entry| entry.kind != FileKind::Directory)
                {
                    return Err(format!(
                        "cannot observe {path} through non-directory ancestor {ancestor}"
                    ));
                }
            }
            Ok(snapshot.entries.get(path))
        }
        ObservationStatus::Missing => Ok(None),
        ObservationStatus::Incomplete { reason } => Err(format!(
            "observation of {} is incomplete: {reason}",
            snapshot.root
        )),
    }
}
pub fn evaluate_filesystem(
    check: &Check,
    current: &[Snapshot],
    configured: Option<&[Snapshot]>,
) -> CheckResult {
    let Some(path) = check.expectation.path() else {
        return result(
            check,
            CheckOutcome::Incomplete,
            "runtime command evidence is required",
        );
    };
    let entry = match observed(path, current) {
        Ok(entry) => entry,
        Err(reason) => return result(check, CheckOutcome::Incomplete, reason),
    };
    let passes = match &check.expectation {
        Expectation::Absent { .. } => entry.is_none(),
        Expectation::Present { expected_kind, .. } => {
            entry.is_some_and(|entry| expected_kind.is_none_or(|kind| kind == entry.kind))
        }
        Expectation::Preserved { .. } => {
            let Some(configured) = configured else {
                return result(
                    check,
                    CheckOutcome::Incomplete,
                    "configured snapshot is unavailable",
                );
            };
            match observed(path, configured) {
                Ok(Some(baseline)) => entry == Some(baseline),
                Ok(None) => false,
                Err(reason) => return result(check, CheckOutcome::Incomplete, reason),
            }
        }
        _ => unreachable!("path expectations handled above"),
    };
    result(
        check,
        if passes {
            CheckOutcome::Passed
        } else {
            CheckOutcome::Failed
        },
        if passes {
            format!("filesystem expectation satisfied: {path}")
        } else {
            format!("filesystem expectation failed: {path}")
        },
    )
}
/// Review findings do not fail a contract. Exact uninstalled path assertions cover only
/// that path: a retention assertion for a directory never silently covers its children.
pub fn review_findings(
    baseline: &[Snapshot],
    final_state: &[Snapshot],
    checks: &[Check],
) -> Vec<Finding> {
    let mut findings = BTreeMap::new();
    let roots = baseline
        .iter()
        .chain(final_state)
        .map(|s| &s.root)
        .collect::<BTreeSet<_>>();
    for root in roots {
        let old = baseline.iter().find(|s| &s.root == root);
        let new = final_state.iter().find(|s| &s.root == root);
        if old.is_none()
            || new.is_none()
            || old
                .into_iter()
                .chain(new)
                .any(|s| matches!(s.completeness, ObservationStatus::Incomplete { .. }))
        {
            findings.insert(
                root.clone(),
                Finding {
                    kind: FindingKind::ObservationIncomplete,
                    path: Some(root.clone()),
                    message: "baseline/final observation is incomplete".into(),
                },
            );
            continue;
        }
        let old = old.expect("checked above");
        let new = new.expect("checked above");
        for path in old
            .entries
            .keys()
            .chain(new.entries.keys())
            .collect::<BTreeSet<_>>()
        {
            let before = old.entries.get(path);
            let after = new.entries.get(path);
            if before == after {
                continue;
            }
            let covered = checks.iter().any(|check| {
                check.checkpoint == Checkpoint::Uninstalled
                    && check.expectation.path() == Some(path)
                    && matches!(
                        (&check.expectation, after),
                        (Expectation::Absent { .. }, None)
                            | (
                                Expectation::Present { .. } | Expectation::Preserved { .. },
                                Some(_)
                            )
                    )
            });
            if covered {
                continue;
            }
            let kind = match (before, after) {
                (None, Some(_)) => FindingKind::Leftover,
                (Some(_), None) => FindingKind::BaselineDeletion,
                _ => FindingKind::ChangedBaseline,
            };
            findings.insert(
                path.clone(),
                Finding {
                    kind,
                    path: Some(path.clone()),
                    message: match kind {
                        FindingKind::Leftover => {
                            "new path remains without a retention/removal assertion"
                        }
                        FindingKind::BaselineDeletion => {
                            "baseline path was deleted without an explicit removal assertion"
                        }
                        _ => "baseline path changed without a retention/removal assertion",
                    }
                    .into(),
                },
            );
        }
    }
    findings.into_values().collect()
}
