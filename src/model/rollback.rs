//! Rollback policy/proof validation shared by live aggregation and offline loading.
use super::*;
use crate::recipe::{RestorationPolicy, RollbackContract, RollbackRecoveryContract};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedFailureEvidence {
    pub expected_exit: i32,
    pub stdout_contains: String,
    pub stdout_matched: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupReference {
    pub source_path: String,
    pub stored_path: String,
}
impl StageReport {
    pub fn verified_failure(&self) -> bool {
        self.step == Some(StepName::AttemptUpgrade)
            && self.name == "attempt_upgrade"
            && self.errors.is_empty()
            && self.commands.len() == 1
            && self.commands.iter().all(|c| {
                valid_invocation(&c.argv, Invocation::ShellScript)
                    && c.phase == "attempt_upgrade"
                    && c.outcome == CommandOutcome::ExpectedFailure
                    && !c.cancelled
                    && !c.timed_out
                    && c.error.is_none()
                    && c.interruption.is_none()
                    && c.rejection.is_none()
                    && c.stdout.as_ref().is_none_or(|log| !log.truncated)
                    && c.expected_failure.as_ref().is_some_and(|p| {
                        (1..=125).contains(&p.expected_exit)
                            && c.exit_status == Some(p.expected_exit)
                            && p.stdout_matched
                            && !p.stdout_contains.trim().is_empty()
                    })
            })
    }
    pub(super) fn unmatched_rollback_evidence(&self) -> bool {
        self.commands.iter().any(|c| {
            (c.expected_failure.is_some() || c.outcome == CommandOutcome::ExpectedFailure)
                && !self.verified_failure()
        }) || self.checks.iter().any(|c| {
            c.backup.is_some()
                && (!c.id.starts_with("__theseus_rollback_")
                    || c.comparison.as_ref().is_none_or(|r| {
                        r.before_stage != "before_upgrade" || r.after_stage != self.name
                    }))
        })
    }
}
impl ScenarioReport {
    pub(super) fn unmatched_rollback_policy(&self) -> bool {
        if self.scenario != ScenarioKind::Rollback {
            return self.rollback.is_some()
                || self.stages.iter().any(|s| {
                    matches!(
                        s.step,
                        Some(
                            StepName::Backup
                                | StepName::AttemptUpgrade
                                | StepName::Rollback
                                | StepName::ExerciseRolledBack
                                | StepName::CleanupBackup
                        )
                    ) || s.checkpoint.is_some_and(|c| {
                        matches!(
                            c,
                            Checkpoint::KnownGood
                                | Checkpoint::BackupCreated
                                | Checkpoint::UpgradeFailed
                                | Checkpoint::RolledBack
                                | Checkpoint::BackupRemoved
                        )
                    }) || s.checks.iter().any(|c| {
                        matches!(
                            c.checkpoint,
                            Checkpoint::KnownGood
                                | Checkpoint::BackupCreated
                                | Checkpoint::UpgradeFailed
                                | Checkpoint::RolledBack
                                | Checkpoint::BackupRemoved
                        )
                    }) || rollback_reference(&s.name)
                        || s.commands.iter().any(|c| {
                            c.expected_failure.is_some()
                                || c.outcome == CommandOutcome::ExpectedFailure
                        })
                        || s.checks.iter().any(|c| {
                            c.backup.is_some()
                                || c.comparison.as_ref().is_some_and(|r| {
                                    rollback_reference(&r.before_stage)
                                        || rollback_reference(&r.after_stage)
                                })
                        })
                });
        }
        let Some(policy) = &self.rollback else {
            return true;
        };
        if crate::recipe::validate_rollback_contract(policy).is_err() || self.downgrade.is_some() {
            return true;
        }
        for stage in &self.stages {
            if stage
                .checkpoint
                .is_some_and(|cp| !checkpoint_applies(ScenarioKind::Rollback, cp))
            {
                return true;
            }
            if stage
                .checks
                .iter()
                .any(|c| !checkpoint_applies(ScenarioKind::Rollback, c.checkpoint))
            {
                return true;
            }
            for command in &stage.commands {
                if (command.expected_failure.is_some()
                    || command.outcome == CommandOutcome::ExpectedFailure)
                    && (!stage.verified_failure()
                        || command.expected_failure.as_ref().is_none_or(|p| {
                            p.expected_exit != policy.failure.expected_exit
                                || p.stdout_contains != policy.failure.stdout_contains
                        }))
                {
                    return true;
                }
            }
            for check in &stage.checks {
                if check.id.starts_with("__theseus_rollback_")
                    && !generated_context(&policy.recovery, stage, check)
                {
                    return true;
                }
                if check.backup.is_some() && !check.id.starts_with("__theseus_rollback_") {
                    return true;
                }
            }
        }
        // Only require the full lifecycle when all observed evidence claims success
        // and teardown is complete; failures and intermediate saves may skip tails.
        let completed_success = matches!(self.teardown, TeardownStatus::Complete)
            && self.errors.is_empty()
            && !self.stages.is_empty()
            && self
                .stages
                .iter()
                .all(|s| s.aggregate_outcome() == RunOutcome::Passed);
        completed_success && !self.complete_rollback(policy)
    }
    fn complete_rollback(&self, policy: &RollbackContract) -> bool {
        let restore = matches!(
            policy.recovery.restoration,
            RestorationPolicy::RestoreData { .. }
        );
        let mut names = vec![
            "baseline",
            "install_base",
            "base_installed",
            "base_installed:after_checks",
            "configure",
            "configured",
            "configured:after_checks",
            "exercise_before_upgrade",
            "known_good",
            "known_good:after_checks",
            "before_upgrade",
        ];
        if restore {
            names.extend(["backup", "backup_created", "backup_created:after_checks"]);
        }
        names.extend([
            "before_attempt",
            "attempt_upgrade",
            "upgrade_failed",
            "upgrade_failed:after_checks",
            "before_rollback",
            "rollback",
            "rolled_back",
            "rolled_back:after_checks",
            "exercise_rolled_back",
        ]);
        if restore {
            names.extend([
                "cleanup_backup",
                "backup_removed",
                "backup_removed:after_checks",
            ]);
        }
        names.extend([
            "uninstall",
            "uninstalled",
            "uninstalled:after_checks",
            "final",
        ]);
        if self
            .stages
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            != names
        {
            return false;
        }
        for stage in &self.stages {
            if stage.snapshots.is_empty()
                || stage.snapshots.iter().any(|s| {
                    matches!(
                        s.completeness,
                        crate::snapshot::ObservationStatus::Incomplete { .. }
                    )
                })
            {
                return false;
            }
            if stage.outcome != StageOutcome::Passed
                || stage
                    .checks
                    .iter()
                    .any(|c| c.outcome != CheckOutcome::Passed)
            {
                return false;
            }
            if let Some(step) = stage.step {
                if step == StepName::AttemptUpgrade {
                    if !stage.verified_failure() {
                        return false;
                    }
                } else if stage.commands.len() != 1
                    || stage.commands.iter().any(|c| {
                        !valid_invocation(&c.argv, Invocation::ShellScript)
                            || c.phase != stage.name
                            || c.exit_status != Some(0)
                            || c.outcome != CommandOutcome::Succeeded
                            || c.expected_failure.is_some()
                            || c.rejection.is_some()
                            || c.interruption.is_some()
                    })
                {
                    return false;
                }
            }
            for check in &stage.checks {
                if check.command_ids.iter().any(|id| {
                    stage
                        .commands
                        .iter()
                        .filter(|c| {
                            &c.id == id
                                && c.phase.ends_with(&format!(":{}", check.id))
                                && c.outcome == CommandOutcome::Succeeded
                                && c.exit_status.is_some()
                                && c.identity.uid == 10001
                                && c.identity.gid == 10001
                        })
                        .count()
                        != 1
                }) {
                    return false;
                }
            }
            for required in required_generated(&policy.recovery, &stage.name) {
                if stage
                    .checks
                    .iter()
                    .filter(|c| {
                        c.id == required
                            && c.outcome == CheckOutcome::Passed
                            && generated_context(&policy.recovery, stage, c)
                    })
                    .count()
                    != 1
                {
                    return false;
                }
            }
        }
        for (name, step) in [
            ("install_base", StepName::InstallBase),
            ("configure", StepName::Configure),
            ("exercise_before_upgrade", StepName::Exercise),
            ("attempt_upgrade", StepName::AttemptUpgrade),
            ("rollback", StepName::Rollback),
            ("exercise_rolled_back", StepName::ExerciseRolledBack),
            ("uninstall", StepName::Uninstall),
        ] {
            if !self
                .stages
                .iter()
                .any(|s| s.name == name && s.step == Some(step))
            {
                return false;
            }
        }
        if restore
            && [
                ("backup", StepName::Backup),
                ("cleanup_backup", StepName::CleanupBackup),
            ]
            .iter()
            .any(|(name, step)| {
                !self
                    .stages
                    .iter()
                    .any(|s| s.name == *name && s.step == Some(*step))
            })
        {
            return false;
        }
        let mut checkpoints = vec![
            Checkpoint::BaseInstalled,
            Checkpoint::Configured,
            Checkpoint::KnownGood,
            Checkpoint::UpgradeFailed,
            Checkpoint::RolledBack,
            Checkpoint::Uninstalled,
        ];
        if restore {
            checkpoints.extend([Checkpoint::BackupCreated, Checkpoint::BackupRemoved]);
        }
        for cp in checkpoints {
            if !self
                .stages
                .iter()
                .any(|s| s.name == cp.to_string() && s.checkpoint == Some(cp))
            {
                return false;
            }
        }
        for (cp, ids) in [
            (Checkpoint::UpgradeFailed, &policy.recovery.change_checks),
            (Checkpoint::RolledBack, &policy.recovery.binary_checks),
            (Checkpoint::RolledBack, &policy.recovery.data_checks),
            (Checkpoint::RolledBack, &policy.recovery.runtime_checks),
        ] {
            for id in ids {
                let found = self
                    .stages
                    .iter()
                    .filter(|s| s.checkpoint == Some(cp))
                    .flat_map(|s| &s.checks)
                    .filter(|c| {
                        &c.id == id && c.checkpoint == cp && c.outcome == CheckOutcome::Passed
                    })
                    .collect::<Vec<_>>();
                if found.len() != 1 {
                    return false;
                }
                let check = found[0];
                if policy.recovery.data_checks.contains(id)
                    || policy.recovery.runtime_checks.contains(id)
                {
                    if check.command_ids.is_empty()
                        || (policy.recovery.data_checks.contains(id)
                            && !reader_evidence(
                                self.stages
                                    .iter()
                                    .find(|s| s.checkpoint == Some(cp))
                                    .unwrap(),
                                check,
                            ))
                    {
                        return false;
                    }
                } else {
                    if check.comparison != ComparisonReference::for_checkpoint(cp) {
                        return false;
                    }
                    let Some(path) = check.path.as_deref() else {
                        return false;
                    };
                    let before_name = if cp == Checkpoint::UpgradeFailed {
                        "before_attempt"
                    } else {
                        "before_upgrade"
                    };
                    let before = &self
                        .stages
                        .iter()
                        .find(|s| s.name == before_name)
                        .unwrap()
                        .snapshots;
                    let after = &self
                        .stages
                        .iter()
                        .find(|s| s.checkpoint == Some(cp))
                        .unwrap()
                        .snapshots;
                    let outcome = if cp == Checkpoint::UpgradeFailed {
                        crate::checks::compare_changed(path, before, after).outcome
                    } else {
                        crate::checks::compare_unchanged(path, before, after).outcome
                    };
                    if outcome != CheckOutcome::Passed
                        || (cp == Checkpoint::RolledBack
                            && !before.iter().any(|s| {
                                s.entries.get(path).is_some_and(|e| {
                                    matches!(e.kind, FileKind::File | FileKind::Symlink)
                                })
                            }))
                    {
                        return false;
                    }
                }
            }
        }
        if let RestorationPolicy::RestoreData {
            backup_root,
            scopes,
        } = &policy.recovery.restoration
        {
            let before = &self
                .stages
                .iter()
                .find(|s| s.name == "before_upgrade")
                .unwrap()
                .snapshots;
            for name in [
                "backup_created",
                "upgrade_failed",
                "before_rollback",
                "rolled_back",
            ] {
                let after = &self
                    .stages
                    .iter()
                    .find(|s| s.name == name)
                    .unwrap()
                    .snapshots;
                for scope in scopes {
                    let stored = format!("{backup_root}/{}", scope.stored);
                    if crate::checks::compare_mapped(&scope.source, &stored, before, after).outcome
                        != CheckOutcome::Passed
                    {
                        return false;
                    }
                    if matches!(name, "backup_created" | "rolled_back")
                        && crate::checks::compare_unchanged(&scope.source, before, after).outcome
                            != CheckOutcome::Passed
                    {
                        return false;
                    }
                }
            }
            for (name, snapshots) in [
                ("backup", before),
                (
                    "backup_removed",
                    &self
                        .stages
                        .iter()
                        .find(|s| s.name == "backup_removed")
                        .unwrap()
                        .snapshots,
                ),
            ] {
                let check = crate::recipe::Check {
                    id: name.into(),
                    checkpoint: Checkpoint::BackupRemoved,
                    expectation: crate::recipe::Expectation::Absent {
                        path: backup_root.clone(),
                    },
                };
                if crate::checks::evaluate_filesystem(&check, snapshots, None).outcome
                    != CheckOutcome::Passed
                {
                    return false;
                }
            }
        }
        if !self.rollback_runtime_bound(policy) {
            return false;
        }
        for cp in [Checkpoint::BaseInstalled, Checkpoint::KnownGood] {
            if !self
                .stages
                .iter()
                .filter(|s| s.checkpoint == Some(cp))
                .any(|stage| {
                    stage.checks.iter().any(|c| {
                        c.checkpoint == cp
                            && c.outcome == CheckOutcome::Passed
                            && if cp == Checkpoint::KnownGood {
                                reader_evidence(stage, c)
                            } else {
                                c.command_ids.len() == 2
                            }
                    })
                })
            {
                return false;
            }
        }
        true
    }
    fn rollback_runtime_bound(&self, policy: &RollbackContract) -> bool {
        let base = self
            .stages
            .iter()
            .find(|s| s.checkpoint == Some(Checkpoint::BaseInstalled))
            .unwrap();
        let restored = self
            .stages
            .iter()
            .find(|s| s.checkpoint == Some(Checkpoint::RolledBack))
            .unwrap();
        let executable =
            |stage: &StageReport, check: &CheckResult| -> Option<(Vec<String>, Vec<String>)> {
                if check.command_ids.len() != 2 {
                    return None;
                }
                let resolve = stage.commands.iter().find(|c| {
                    c.id == check.command_ids[0] && c.phase == format!("resolve:{}", check.id)
                })?;
                let run = stage.commands.iter().find(|c| {
                    c.id == check.command_ids[1] && c.phase == format!("check:{}", check.id)
                })?;
                if resolve.exit_status != Some(0)
                    || run.exit_status != Some(0)
                    || !valid_invocation(&resolve.argv, Invocation::Resolve)
                    || !valid_invocation(&run.argv, Invocation::Executable)
                {
                    return None;
                }
                Some((resolve.argv.clone(), run.argv.clone()))
            };
        policy.recovery.runtime_checks.iter().all(|id| {
            let Some(check) = restored.checks.iter().find(|c| &c.id == id) else {
                return false;
            };
            let Some((resolve, run)) = executable(restored, check) else {
                return false;
            };
            policy.recovery.binary_checks.iter().any(|id| {
                restored
                    .checks
                    .iter()
                    .any(|c| &c.id == id && c.path.as_ref() == run.first())
            }) && base.checks.iter().any(|c| {
                executable(base, c)
                    .is_some_and(|signature| signature == (resolve.clone(), run.clone()))
            })
        })
    }
}
/// The normal executor records one of these generated argv shapes. This checks
/// structural executability, not equality to an unavailable original author script.
enum Invocation {
    ShellScript,
    Resolve,
    Executable,
}
fn valid_invocation(argv: &[String], role: Invocation) -> bool {
    if argv.is_empty() || argv.iter().any(|arg| arg.contains('\0')) {
        return false;
    }
    match role {
        Invocation::ShellScript => matches!(argv, [shell, flag, script]
            if shell == "/bin/sh" && flag == "-c" && !script.trim().is_empty()),
        Invocation::Resolve => matches!(argv, [shell, flag, script, label, name]
            if shell == "/bin/sh" && flag == "-c" && script == "command -v \"$1\""
                && label == "theseus-resolve" && !name.is_empty()
                && !name.contains('/') && !name.chars().any(char::is_whitespace)),
        Invocation::Executable => {
            let path = &argv[0];
            path != "/"
                && crate::recipe::normalize_container_path(path)
                    .is_ok_and(|normalized| &normalized == path)
        }
    }
}
fn reader_evidence(stage: &StageReport, check: &CheckResult) -> bool {
    check.command_ids.len() == 1
        && stage.commands.iter().any(|c| {
            valid_invocation(&c.argv, Invocation::ShellScript)
                && c.id == check.command_ids[0]
                && c.phase == format!("check:{}", check.id)
                && c.exit_status == Some(0)
                && c.outcome == CommandOutcome::Succeeded
        })
}
fn required_generated(policy: &RollbackRecoveryContract, name: &str) -> Vec<String> {
    let RestorationPolicy::RestoreData { scopes, .. } = &policy.restoration else {
        return vec![];
    };
    let labels: &[&str] = match name {
        "backup" => return vec!["__theseus_rollback_root_absent".into()],
        "backup_created" => &["source_stable", "backup_matches"],
        "upgrade_failed" => &["backup_intact"],
        "before_rollback" => &["backup_ready"],
        "rolled_back" => &["restored", "backup_retained"],
        "backup_removed" => return vec!["__theseus_rollback_backup_removed".into()],
        _ => &[],
    };
    scopes
        .iter()
        .flat_map(|s| {
            labels
                .iter()
                .map(move |l| format!("__theseus_rollback_{l}_{}", s.id))
        })
        .collect()
}
fn generated_context(
    policy: &RollbackRecoveryContract,
    stage: &StageReport,
    check: &CheckResult,
) -> bool {
    let RestorationPolicy::RestoreData {
        backup_root,
        scopes,
    } = &policy.restoration
    else {
        return false;
    };
    if !required_generated(policy, &stage.name).contains(&check.id) {
        return false;
    }
    let checkpoint = match stage.name.as_str() {
        "backup" | "backup_created" => Checkpoint::BackupCreated,
        "upgrade_failed" | "before_rollback" => Checkpoint::UpgradeFailed,
        "rolled_back" => Checkpoint::RolledBack,
        "backup_removed" => Checkpoint::BackupRemoved,
        _ => return false,
    };
    if check.checkpoint != checkpoint {
        return false;
    }
    let after = if stage.name == "backup" {
        "before_upgrade"
    } else {
        &stage.name
    };
    if check
        .comparison
        .as_ref()
        .is_none_or(|r| r.before_stage != "before_upgrade" || r.after_stage != after)
    {
        return false;
    }
    if matches!(stage.name.as_str(), "backup" | "backup_removed") {
        return check.path.as_ref() == Some(backup_root) && check.backup.is_none();
    }
    scopes.iter().any(|s| {
        check.path.as_ref() == Some(&s.source)
            && check.backup.as_ref().is_some_and(|b| {
                b.source_path == s.source && b.stored_path == format!("{backup_root}/{}", s.stored)
            })
            && [
                "source_stable",
                "backup_matches",
                "backup_intact",
                "backup_ready",
                "restored",
                "backup_retained",
            ]
            .iter()
            .any(|label| check.id == format!("__theseus_rollback_{label}_{}", s.id))
    })
}
/// Every new observation reference has a report-five feature floor.
pub(crate) fn rollback_reference(stage: &str) -> bool {
    matches!(
        stage,
        "before_upgrade"
            | "before_attempt"
            | "before_rollback"
            | "known_good"
            | "backup_created"
            | "upgrade_failed"
            | "rolled_back"
            | "backup_removed"
    )
}
