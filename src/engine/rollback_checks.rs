//! Independent rollback filesystem evidence, emitted as ordinary check records.
use crate::{
    checks,
    model::*,
    recipe::{Check, Expectation, RestorationPolicy, RollbackContract},
    snapshot::Snapshot,
};

fn result(
    id: String,
    checkpoint: Checkpoint,
    evidence: checks::StabilityResult,
    path: &str,
    stored: Option<&str>,
    after: &str,
) -> CheckResult {
    CheckResult {
        id,
        checkpoint,
        outcome: evidence.outcome,
        message: evidence.message,
        path: Some(path.into()),
        command_ids: vec![],
        comparison: Some(ComparisonReference {
            before_stage: "before_upgrade".into(),
            after_stage: after.into(),
        }),
        backup: stored.map(|s| BackupReference {
            source_path: path.into(),
            stored_path: s.into(),
        }),
    }
}
pub(super) fn root_absent(
    root: &str,
    snapshots: &[Snapshot],
    checkpoint: Checkpoint,
    after: &str,
) -> CheckResult {
    let check = Check {
        id: if checkpoint == Checkpoint::BackupRemoved {
            "__theseus_rollback_backup_removed"
        } else {
            "__theseus_rollback_root_absent"
        }
        .into(),
        checkpoint,
        expectation: Expectation::Absent { path: root.into() },
    };
    let mut r = checks::evaluate_filesystem(&check, snapshots, None);
    r.comparison = Some(ComparisonReference {
        before_stage: "before_upgrade".into(),
        after_stage: after.into(),
    });
    r
}
pub(super) fn derived(
    policy: &RollbackContract,
    checkpoint: Checkpoint,
    before: &[Snapshot],
    after: &[Snapshot],
    phase: &str,
) -> Vec<CheckResult> {
    let RestorationPolicy::RestoreData {
        backup_root,
        scopes,
    } = &policy.restoration
    else {
        return vec![];
    };
    if checkpoint == Checkpoint::BackupRemoved {
        return vec![root_absent(backup_root, after, checkpoint, phase)];
    }
    let mut results = vec![];
    for scope in scopes {
        let stored = format!("{backup_root}/{}", scope.stored);
        let prefix = match (checkpoint, phase) {
            (Checkpoint::BackupCreated, _) => "backup_matches",
            (Checkpoint::UpgradeFailed, "before_rollback") => "backup_ready",
            (Checkpoint::UpgradeFailed, _) => "backup_intact",
            (Checkpoint::RolledBack, _) => "backup_retained",
            _ => continue,
        };
        if matches!(
            checkpoint,
            Checkpoint::BackupCreated | Checkpoint::RolledBack
        ) {
            let label = if checkpoint == Checkpoint::BackupCreated {
                "source_stable"
            } else {
                "restored"
            };
            results.push(result(
                format!("__theseus_rollback_{label}_{}", scope.id),
                checkpoint,
                checks::compare_unchanged(&scope.source, before, after),
                &scope.source,
                Some(&stored),
                phase,
            ));
        }
        results.push(result(
            format!("__theseus_rollback_{prefix}_{}", scope.id),
            checkpoint,
            checks::compare_mapped(&scope.source, &stored, before, after),
            &scope.source,
            Some(&stored),
            phase,
        ));
    }
    results
}
