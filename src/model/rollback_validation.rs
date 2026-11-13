//! Common rollback observation, role and runtime binding validation.
use super::*;
use crate::recipe::{RestorationPolicy, RollbackRecoveryContract};
impl ScenarioReport {
    pub(super) fn complete_rollback_recovery(
        &self,
        policy: &RollbackRecoveryContract,
        change_checkpoint: Checkpoint,
    ) -> bool {
        let restore = matches!(policy.restoration, RestorationPolicy::RestoreData { .. });
        let interrupted = change_checkpoint == Checkpoint::Interrupted;
        let change_name = if interrupted {
            "interrupted"
        } else {
            "upgrade_failed"
        };
        let before_name = if interrupted {
            "before_interrupt"
        } else {
            "before_attempt"
        };
        for stage in &self.stages {
            if interrupted && !interrupted_stage_layout(stage) {
                return false;
            }
            if !rollback_stage_commands(stage, interrupted) {
                return false;
            }
            if !self.rollback_stage_evidence(policy, change_checkpoint, interrupted, stage) {
                return false;
            }
        }
        if !self.rollback_stages_present(interrupted, restore, change_checkpoint) {
            return false;
        }
        if !self.rollback_policy_checks(policy, change_checkpoint, interrupted, before_name) {
            return false;
        }
        if !self.rollback_restored_data(policy, change_name) {
            return false;
        }
        if !self.rollback_runtime_bound(policy) {
            return false;
        }
        self.rollback_baseline_evidence(interrupted)
    }
    fn rollback_stage_evidence(
        &self,
        policy: &RollbackRecoveryContract,
        change_checkpoint: Checkpoint,
        interrupted: bool,
        stage: &StageReport,
    ) -> bool {
        if interrupted && stage.step.is_some() {
            let runtime = matches!(
                stage.step,
                Some(StepName::Configure | StepName::Exercise | StepName::ExerciseRolledBack)
            );
            let identity = if runtime || self.profile == ProfileName::User {
                ExecutionIdentity::runtime()
            } else {
                ExecutionIdentity::root()
            };
            if stage.commands.iter().any(|c| {
                c.identity.uid != identity.uid
                    || c.identity.gid != identity.gid
                    || c.identity.home != identity.home
            }) {
                return false;
            }
        }
        for check in &stage.checks {
            if interrupted
                && check.command_expectation.is_some()
                && !command_role_evidence(stage, check)
            {
                return false;
            }
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
        for required in required_generated(policy, &stage.name) {
            if stage
                .checks
                .iter()
                .filter(|c| {
                    c.id == required
                        && c.outcome == CheckOutcome::Passed
                        && generated_context_at(policy, stage, c, change_checkpoint)
                })
                .count()
                != 1
            {
                return false;
            }
        }
        true
    }
    fn rollback_stages_present(
        &self,
        interrupted: bool,
        restore: bool,
        change_checkpoint: Checkpoint,
    ) -> bool {
        for (name, step) in [
            ("install_base", StepName::InstallBase),
            ("configure", StepName::Configure),
            ("exercise_before_upgrade", StepName::Exercise),
            (
                if interrupted {
                    "interrupt_upgrade"
                } else {
                    "attempt_upgrade"
                },
                if interrupted {
                    StepName::InterruptUpgrade
                } else {
                    StepName::AttemptUpgrade
                },
            ),
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
            change_checkpoint,
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
        true
    }
    fn rollback_policy_checks(
        &self,
        policy: &RollbackRecoveryContract,
        change_checkpoint: Checkpoint,
        interrupted: bool,
        before_name: &str,
    ) -> bool {
        for (cp, ids) in [
            (change_checkpoint, &policy.change_checks),
            (Checkpoint::RolledBack, &policy.binary_checks),
            (Checkpoint::RolledBack, &policy.data_checks),
            (Checkpoint::RolledBack, &policy.runtime_checks),
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
                if policy.data_checks.contains(id) || policy.runtime_checks.contains(id) {
                    if interrupted
                        && !command_role_evidence(
                            self.stages
                                .iter()
                                .find(|s| s.checkpoint == Some(cp))
                                .unwrap(),
                            check,
                        )
                    {
                        return false;
                    }
                    if check.command_ids.is_empty()
                        || (policy.data_checks.contains(id)
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
                    let before_name = if cp == change_checkpoint {
                        before_name
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
                    let outcome = if cp == change_checkpoint {
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
        true
    }
    fn rollback_restored_data(&self, policy: &RollbackRecoveryContract, change_name: &str) -> bool {
        if let RestorationPolicy::RestoreData {
            backup_root,
            scopes,
        } = &policy.restoration
        {
            let before = &self
                .stages
                .iter()
                .find(|s| s.name == "before_upgrade")
                .unwrap()
                .snapshots;
            for name in [
                "backup_created",
                change_name,
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
        true
    }
    fn rollback_baseline_evidence(&self, interrupted: bool) -> bool {
        for cp in [Checkpoint::BaseInstalled, Checkpoint::KnownGood] {
            if !self
                .stages
                .iter()
                .filter(|s| s.checkpoint == Some(cp))
                .any(|stage| {
                    stage.checks.iter().any(|c| {
                        c.checkpoint == cp
                            && c.outcome == CheckOutcome::Passed
                            && (!interrupted || command_role_evidence(stage, c))
                            && if cp == Checkpoint::KnownGood {
                                reader_evidence(stage, c)
                                    && (!interrupted
                                        || c.command_expectation
                                            .as_ref()
                                            .is_some_and(|p| p.executable.is_none())
                                            && command_role_evidence(stage, c))
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
    fn rollback_runtime_bound(&self, policy: &RollbackRecoveryContract) -> bool {
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
        policy.runtime_checks.iter().all(|id| {
            let Some(check) = restored.checks.iter().find(|c| &c.id == id) else {
                return false;
            };
            let Some((resolve, run)) = executable(restored, check) else {
                return false;
            };
            policy.binary_checks.iter().any(|id| {
                restored
                    .checks
                    .iter()
                    .any(|c| &c.id == id && c.path.as_ref() == run.first())
            }) && base.checks.iter().any(|c| {
                if self.scenario == ScenarioKind::InterruptedRollback
                    && (c.command_expectation.is_none()
                        || c.command_expectation != check.command_expectation
                        || !command_role_evidence(base, c))
                {
                    return false;
                }
                executable(base, c)
                    .is_some_and(|signature| signature == (resolve.clone(), run.clone()))
            })
        })
    }
}
fn interrupted_stage_layout(stage: &StageReport) -> bool {
    let step = match stage.name.as_str() {
        "install_base" => Some(StepName::InstallBase),
        "configure" => Some(StepName::Configure),
        "exercise_before_upgrade" => Some(StepName::Exercise),
        "backup" => Some(StepName::Backup),
        "interrupt_upgrade" => Some(StepName::InterruptUpgrade),
        "rollback" => Some(StepName::Rollback),
        "exercise_rolled_back" => Some(StepName::ExerciseRolledBack),
        "cleanup_backup" => Some(StepName::CleanupBackup),
        "uninstall" => Some(StepName::Uninstall),
        _ => None,
    };
    let checkpoint = [
        Checkpoint::BaseInstalled,
        Checkpoint::Configured,
        Checkpoint::KnownGood,
        Checkpoint::BackupCreated,
        Checkpoint::Interrupted,
        Checkpoint::RolledBack,
        Checkpoint::BackupRemoved,
        Checkpoint::Uninstalled,
    ]
    .into_iter()
    .find(|cp| cp.to_string() == stage.name);
    if stage.step != step
        || stage.checkpoint != checkpoint
        || (step.is_none() && checkpoint.is_none() && !stage.commands.is_empty())
    {
        return false;
    }
    if checkpoint.is_some()
        && stage.commands.iter().any(|command| {
            stage
                .checks
                .iter()
                .flat_map(|c| &c.command_ids)
                .filter(|id| *id == &command.id)
                .count()
                != 1
        })
    {
        return false;
    }
    true
}
fn rollback_stage_commands(stage: &StageReport, interrupted: bool) -> bool {
    if !matches!(
        stage.name.as_str(),
        "interrupt_upgrade" | "restart_after_interrupt"
    ) && (stage.snapshots.is_empty()
        || stage.snapshots.iter().any(|s| {
            matches!(
                s.completeness,
                crate::snapshot::ObservationStatus::Incomplete { .. }
            )
        }))
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
        if interrupted && step == StepName::InterruptUpgrade {
            if !stage.verified_interruption()
                || !stage.commands.iter().all(|c| {
                    valid_invocation(&c.argv, Invocation::ShellScript)
                        && c.expected_failure.is_none()
                        && c.rejection.is_none()
                })
            {
                return false;
            }
        } else if step == StepName::AttemptUpgrade {
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
    true
}
/// The normal executor records one of these generated argv shapes. This checks
/// structural executability, not equality to an unavailable original author script.
pub(super) enum Invocation {
    ShellScript,
    Resolve,
    Executable,
}
pub(super) fn valid_invocation(argv: &[String], role: Invocation) -> bool {
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
pub(super) fn reader_evidence(stage: &StageReport, check: &CheckResult) -> bool {
    check.command_ids.len() == 1
        && stage.commands.iter().any(|c| {
            valid_invocation(&c.argv, Invocation::ShellScript)
                && c.id == check.command_ids[0]
                && c.phase == format!("check:{}", check.id)
                && c.exit_status == Some(0)
                && c.outcome == CommandOutcome::Succeeded
        })
}
pub(super) fn required_generated(policy: &RollbackRecoveryContract, name: &str) -> Vec<String> {
    let RestorationPolicy::RestoreData { scopes, .. } = &policy.restoration else {
        return vec![];
    };
    let labels: &[&str] = match name {
        "backup" => return vec!["__theseus_rollback_root_absent".into()],
        "backup_created" => &["source_stable", "backup_matches"],
        "upgrade_failed" | "interrupted" => &["backup_intact"],
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
pub(super) fn generated_context(
    policy: &RollbackRecoveryContract,
    stage: &StageReport,
    check: &CheckResult,
) -> bool {
    generated_context_at(policy, stage, check, Checkpoint::UpgradeFailed)
}
pub(super) fn generated_context_at(
    policy: &RollbackRecoveryContract,
    stage: &StageReport,
    check: &CheckResult,
    change_checkpoint: Checkpoint,
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
        "upgrade_failed" | "interrupted" | "before_rollback" => change_checkpoint,
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

pub(super) fn command_role_evidence(stage: &StageReport, check: &CheckResult) -> bool {
    let Some(proof) = &check.command_expectation else {
        return false;
    };
    if proof.expected_exit != 0
        || proof.stdout_contains.trim().is_empty()
        || !proof.stdout_matched
        || proof.stdout_match.as_deref() != Some(proof.stdout_contains.as_str())
    {
        return false;
    }
    let Some(run) = check
        .command_ids
        .last()
        .and_then(|id| stage.commands.iter().find(|c| &c.id == id))
    else {
        return false;
    };
    if run.phase != format!("check:{}", check.id)
        || run.exit_status != Some(proof.expected_exit)
        || run.outcome != CommandOutcome::Succeeded
        || run.error.is_some()
        || run.timed_out
        || run.cancelled
        || run.expected_failure.is_some()
        || run.rejection.is_some()
        || run.interruption.is_some()
        || run.stdout.as_ref().is_none_or(|l| {
            l.truncated || l.bytes == 0 || proof.stdout_contains.len() as u64 > l.bytes
        })
        || run.identity.uid != 10001
        || run.identity.gid != 10001
        || run.identity.home != "/home/theseus"
    {
        return false;
    }
    match &proof.executable {
        None => {
            check.command_ids.len() == 1 && valid_invocation(&run.argv, Invocation::ShellScript)
        }
        Some(executable) => {
            if check.command_ids.len() != 2 || !valid_invocation(&run.argv, Invocation::Executable)
            {
                return false;
            }
            let Some(resolve) = stage.commands.iter().find(|c| c.id == check.command_ids[0]) else {
                return false;
            };
            let mut expected = vec![executable.resolved_path.clone()];
            expected.extend(executable.args.clone());
            run.argv == expected
                && valid_invocation(&resolve.argv, Invocation::Resolve)
                && resolve.argv.last() == Some(&executable.name)
                && resolve.phase == format!("resolve:{}", check.id)
                && resolve.exit_status == Some(0)
                && resolve.outcome == CommandOutcome::Succeeded
                && resolve.error.is_none()
                && !resolve.cancelled
                && !resolve.timed_out
                && resolve
                    .stdout
                    .as_ref()
                    .is_some_and(|l| !l.truncated && l.bytes > 0)
                && resolve.identity.uid == 10001
                && resolve.identity.gid == 10001
                && resolve.identity.home == "/home/theseus"
                && resolve.expected_failure.is_none()
                && resolve.rejection.is_none()
                && resolve.interruption.is_none()
        }
    }
}
