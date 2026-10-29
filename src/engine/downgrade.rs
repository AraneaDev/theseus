//! Explicit downgrade policy orchestration using the shared lifecycle boundary.
use super::{
    stages::{Availability, ScenarioContext, State, observed},
    *,
};
use crate::{checks, recipe::DowngradeContract};

pub(super) fn run<E: Environment, F: FnMut(EngineEvent)>(
    c: &mut ScenarioContext<'_, E, F>,
    state: &mut State,
) -> Result<()> {
    let policy = c.report.scenarios[c.index]
        .downgrade
        .clone()
        .expect("validated downgrade policy");
    c.step(state, StepName::InstallTarget)?;
    c.checkpoint(state, Checkpoint::TargetInstalled, false)?;
    c.step(state, StepName::Configure)?;
    c.checkpoint(state, Checkpoint::Configured, true)?;
    c.step_named(state, StepName::Exercise, "exercise_before_downgrade")?;
    let mut reference = StageReport::new("before_downgrade");
    c.capture(state, &mut reference);
    state.before_downgrade = Some(reference.snapshots.clone());
    c.finish(reference)?;
    match &policy {
        DowngradeContract::Compatible { .. } => c.step(state, StepName::Downgrade)?,
        DowngradeContract::SafeRejection {
            expected_exit,
            stdout_contains,
            ..
        } => {
            if !state.forward
                || state.availability != Availability::Running
                || c.cancel.is_cancelled()
            {
                c.step(state, StepName::Downgrade)?;
            } else {
                let mut stage = StageReport::new("downgrade");
                stage.step = Some(StepName::Downgrade);
                c.start(&stage)?;
                let profile = c.report.scenarios[c.index].profile;
                let spec = c.recipe.recipe.profiles[&profile].steps[&StepName::Downgrade].clone();
                let identity = if profile == ProfileName::System {
                    ExecutionIdentity::root()
                } else {
                    ExecutionIdentity::runtime()
                };
                let request = c.request(
                    state,
                    "downgrade".into(),
                    &spec,
                    identity,
                    vec!["/bin/sh".into(), "-c".into(), spec.script.clone()],
                    *expected_exit,
                );
                let mut executed = c.execute(state, request, &mut stage);
                let assessment =
                    checks::assess_rejection(*expected_exit, stdout_contains, &observed(&executed));
                if !executed.record.cancelled && !executed.record.timed_out {
                    executed.record.outcome = match assessment.outcome {
                        CheckOutcome::Passed => {
                            executed.record.rejection = Some(RejectionEvidence {
                                expected_exit: *expected_exit,
                                stdout_contains: stdout_contains.clone(),
                                stdout_matched: true,
                            });
                            CommandOutcome::ExpectedRejection
                        }
                        CheckOutcome::Failed => CommandOutcome::Failed,
                        _ => CommandOutcome::Error,
                    };
                }
                // execute stores a clone: persist the classified record, not just
                // the transient runner return, before aggregation or any save.
                *stage.commands.last_mut().expect("execute appended record") = executed.record;
                state.forward &= stage.verified_rejection();
                c.capture(state, &mut stage);
                ScenarioContext::<E, F>::outcome(&mut stage);
                c.update(&stage)?;
            }
        }
    }
    let (checkpoint, step, phase) = match policy {
        DowngradeContract::Compatible { .. } => (
            Checkpoint::Downgraded,
            StepName::ExerciseDowngraded,
            "exercise_downgraded",
        ),
        DowngradeContract::SafeRejection { .. } => (
            Checkpoint::DowngradeRejected,
            StepName::Exercise,
            "exercise_after_rejection",
        ),
    };
    c.checkpoint(state, checkpoint, false)?;
    c.step_named(state, step, phase)
}
