//! One failed candidate and an explicitly verified rollback, using normal execution.
use super::{
    stages::{Availability, ScenarioContext, State, observed},
    *,
};
use crate::{checks, recipe::RestorationPolicy};

pub(super) fn reference<E: Environment, F: FnMut(EngineEvent)>(
    c: &mut ScenarioContext<'_, E, F>,
    state: &mut State,
    name: &str,
) -> Result<Vec<crate::snapshot::Snapshot>> {
    let mut stage = StageReport::new(name);
    c.capture(state, &mut stage);
    ScenarioContext::<E, F>::outcome(&mut stage);
    state.forward &= stage.aggregate_outcome() == RunOutcome::Passed && !stage.snapshots.is_empty();
    let snapshots = stage.snapshots.clone();
    c.finish(stage)?;
    Ok(snapshots)
}
pub(super) fn run<E: Environment, F: FnMut(EngineEvent)>(
    c: &mut ScenarioContext<'_, E, F>,
    state: &mut State,
) -> Result<()> {
    let policy = c.report.scenarios[c.index]
        .rollback
        .clone()
        .expect("validated rollback policy");
    prepare(c, state, &policy.recovery)?;
    state.before_attempt = Some(reference(c, state, "before_attempt")?);
    if state.forward && state.availability == Availability::Running && !c.cancel.is_cancelled() {
        let mut stage = StageReport::new("attempt_upgrade");
        stage.step = Some(StepName::AttemptUpgrade);
        c.start(&stage)?;
        let profile = c.report.scenarios[c.index].profile;
        let spec = c.recipe.recipe.profiles[&profile].steps[&StepName::AttemptUpgrade].clone();
        let identity = if profile == ProfileName::System {
            ExecutionIdentity::root()
        } else {
            ExecutionIdentity::runtime()
        };
        let request = c.request(
            state,
            "attempt_upgrade".into(),
            &spec,
            identity,
            vec!["/bin/sh".into(), "-c".into(), spec.script.clone()],
            policy.failure.expected_exit,
        );
        let mut executed = c.execute(state, request, &mut stage);
        let assessment = checks::assess_expected_failure(&policy.failure, &observed(&executed));
        if !executed.record.cancelled && !executed.record.timed_out {
            executed.record.outcome = match assessment.outcome {
                CheckOutcome::Passed => {
                    executed.record.expected_failure = Some(ExpectedFailureEvidence {
                        expected_exit: policy.failure.expected_exit,
                        stdout_contains: policy.failure.stdout_contains.clone(),
                        stdout_matched: true,
                    });
                    CommandOutcome::ExpectedFailure
                }
                CheckOutcome::Failed => CommandOutcome::Failed,
                _ => CommandOutcome::Error,
            };
        }
        *stage.commands.last_mut().expect("execute appended") = executed.record;
        state.forward &= stage.verified_failure();
        c.capture(state, &mut stage);
        ScenarioContext::<E, F>::outcome(&mut stage);
        c.update(&stage)?;
    } else {
        c.step(state, StepName::AttemptUpgrade)?;
    }
    c.checkpoint(state, Checkpoint::UpgradeFailed, false)?;
    restore(c, state, &policy.recovery, Checkpoint::UpgradeFailed)
}
pub(super) fn prepare<E: Environment, F: FnMut(EngineEvent)>(
    c: &mut ScenarioContext<'_, E, F>,
    state: &mut State,
    policy: &crate::recipe::RollbackRecoveryContract,
) -> Result<()> {
    c.step(state, StepName::InstallBase)?;
    c.checkpoint(state, Checkpoint::BaseInstalled, false)?;
    c.step(state, StepName::Configure)?;
    c.checkpoint(state, Checkpoint::Configured, true)?;
    c.step_named(state, StepName::Exercise, "exercise_before_upgrade")?;
    c.checkpoint(state, Checkpoint::KnownGood, false)?;
    state.before_upgrade = Some(reference(c, state, "before_upgrade")?);
    if let RestorationPolicy::RestoreData { backup_root, .. } = &policy.restoration {
        let mut stage = StageReport::new("backup");
        stage.step = Some(StepName::Backup);
        if state.forward && state.availability == Availability::Running && !c.cancel.is_cancelled()
        {
            let check = super::rollback_checks::root_absent(
                backup_root,
                state.before_upgrade.as_deref().unwrap_or_default(),
                Checkpoint::BackupCreated,
                "before_upgrade",
            );
            state.forward &= check.outcome == CheckOutcome::Passed;
            stage.checks.push(check);
            if !state.forward {
                c.finish(stage)?;
            } else {
                // Publish the pre-command result before executing trusted backup.
                c.start(&stage)?;
                if !c.cancel.is_cancelled() {
                    state.rollback_backup_authorized = true;
                    c.execute_step(state, StepName::Backup, "backup", &mut stage);
                } else {
                    stage.outcome = StageOutcome::Cancelled;
                }
                c.capture(state, &mut stage);
                ScenarioContext::<E, F>::outcome(&mut stage);
                c.update(&stage)?;
            }
        } else {
            c.step(state, StepName::Backup)?;
        }
        c.checkpoint(state, Checkpoint::BackupCreated, false)?;
    }
    Ok(())
}
pub(super) fn restore<E: Environment, F: FnMut(EngineEvent)>(
    c: &mut ScenarioContext<'_, E, F>,
    state: &mut State,
    policy: &crate::recipe::RollbackRecoveryContract,
    change_checkpoint: Checkpoint,
) -> Result<()> {
    let mut ready = StageReport::new("before_rollback");
    c.capture(state, &mut ready);
    {
        ready.checks = super::rollback_checks::derived(
            policy,
            change_checkpoint,
            state.before_upgrade.as_deref().unwrap_or_default(),
            &ready.snapshots,
            "before_rollback",
        );
        if !state.forward {
            for check in &mut ready.checks {
                check.outcome = CheckOutcome::Skipped;
                check.message = "failed prerequisite prevents backup revalidation".into();
            }
        }
        state.forward &= ready
            .checks
            .iter()
            .all(|r| r.outcome == CheckOutcome::Passed);
    }
    ScenarioContext::<E, F>::outcome(&mut ready);
    state.forward &= ready.aggregate_outcome() == RunOutcome::Passed && !ready.snapshots.is_empty();
    c.finish(ready)?;
    c.step(state, StepName::Rollback)?;
    c.checkpoint(state, Checkpoint::RolledBack, false)?;
    c.step(state, StepName::ExerciseRolledBack)?;
    if matches!(policy.restoration, RestorationPolicy::RestoreData { .. }) {
        c.step(state, StepName::CleanupBackup)?;
        c.checkpoint(state, Checkpoint::BackupRemoved, false)?;
    }
    Ok(())
}
