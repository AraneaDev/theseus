//! Deliberate stop and forward recovery share the normal engine's evidence helpers.
use super::{
    stages::{Availability, ScenarioContext, State},
    *,
};

pub(super) fn run<E: Environment, F: FnMut(EngineEvent)>(
    c: &mut ScenarioContext<'_, E, F>,
    state: &mut State,
    kind: ScenarioKind,
) -> Result<()> {
    let (interrupt, recover) = match kind {
        ScenarioKind::InterruptedInstall => (StepName::InterruptInstall, StepName::RecoverInstall),
        ScenarioKind::InterruptedUpgrade => {
            c.step(state, StepName::InstallBase)?;
            c.checkpoint(state, Checkpoint::BaseInstalled, false)?;
            c.step(state, StepName::Configure)?;
            c.checkpoint(state, Checkpoint::Configured, true)?;
            c.step_named(state, StepName::Exercise, "exercise_before_interrupt")?;
            (StepName::InterruptUpgrade, StepName::RecoverUpgrade)
        }
        _ => unreachable!("only interruption scenarios"),
    };
    let mut reference = StageReport::new("before_interrupt");
    c.capture(state, &mut reference);
    state.before_interrupt = Some(reference.snapshots.clone());
    c.finish(reference)?;
    stop(c, state, interrupt)?;
    restart(c, state)?;
    c.checkpoint(state, Checkpoint::Interrupted, false)?;
    c.step(state, recover)?;
    c.checkpoint(state, Checkpoint::Recovered, false)?;
    if kind == ScenarioKind::InterruptedInstall {
        c.step(state, StepName::Configure)?;
        c.checkpoint(state, Checkpoint::Configured, true)?;
    }
    c.step_named(state, StepName::Exercise, "exercise_after_recovery")
}
pub(super) fn stop<E: Environment, F: FnMut(EngineEvent)>(
    c: &mut ScenarioContext<'_, E, F>,
    state: &mut State,
    step: StepName,
) -> Result<()> {
    let mut stage = StageReport::new(step.to_string());
    stage.step = Some(step);
    if c.cancel.is_cancelled() {
        state.availability = Availability::Unavailable;
        c.report.cancelled = true;
    }
    if state.availability != Availability::Running || !state.forward {
        stage.outcome = StageOutcome::Skipped;
        stage.skip_reason = Some("failed prerequisite or unavailable container".into());
        return c.finish(stage);
    }
    c.start(&stage)?;
    if c.cancel.is_cancelled() {
        state.availability = Availability::Unavailable;
        state.forward = false;
        c.report.cancelled = true;
        stage.outcome = StageOutcome::Cancelled;
        return c.update(&stage);
    }
    let profile = c.report.scenarios[c.index].profile;
    let spec = c.recipe.recipe.profiles[&profile].steps[&step].clone();
    let identity = if profile == ProfileName::System {
        ExecutionIdentity::root()
    } else {
        ExecutionIdentity::runtime()
    };
    let request = c.request(
        state,
        step.to_string(),
        &spec,
        identity,
        vec!["/bin/sh".into(), "-c".into(), spec.script.clone()],
        0,
    );
    match c
        .environment
        .execute_interrupted(&state.resource, request, c.store, c.cancel)
    {
        Ok(executed) => {
            stage.commands.push(executed.record);
        }
        Err(error) => {
            stage.errors.push(format!("interruption: {error:#}"));
        }
    }
    if c.cancel.is_cancelled() || stage.commands.iter().any(|command| command.cancelled) {
        c.cancel.cancel();
        c.report.cancelled = true;
        stage.outcome = StageOutcome::Cancelled;
        state.availability = Availability::Unavailable;
        state.forward = false;
    } else if stage.verified_interruption()
        && (c.report.scenarios[c.index].scenario != ScenarioKind::InterruptedRollback
            || stage.verified_rollback_interruption())
    {
        state.availability = Availability::Stopped;
    } else {
        state.forward = false;
        // Only independently verified ordinary completion establishes a running target.
        let completed = stage.commands.first().is_some_and(|command| {
            command.outcome == CommandOutcome::Failed
                && command.exit_status.is_some()
                && !command.timed_out
                && !command.cancelled
                && command.error.is_none()
                && command
                    .interruption
                    .as_ref()
                    .is_some_and(|proof| !proof.readiness_verified && !proof.stopped_verified)
        });
        if !completed {
            state.availability = Availability::Unavailable;
        }
    }
    if stage.outcome != StageOutcome::Cancelled {
        ScenarioContext::<E, F>::outcome(&mut stage);
    }
    c.update(&stage)
}
pub(super) fn restart<E: Environment, F: FnMut(EngineEvent)>(
    c: &mut ScenarioContext<'_, E, F>,
    state: &mut State,
) -> Result<()> {
    let mut stage = StageReport::new("restart_after_interrupt");
    if c.cancel.is_cancelled() {
        state.availability = Availability::Unavailable;
        c.report.cancelled = true;
    }
    if state.availability != Availability::Stopped {
        stage.outcome = StageOutcome::Skipped;
        stage.skip_reason = Some("no verified planned stop".into());
        return c.finish(stage);
    }
    c.start(&stage)?;
    if c.cancel.is_cancelled() {
        state.availability = Availability::Unavailable;
        state.forward = false;
        c.report.cancelled = true;
        stage.outcome = StageOutcome::Cancelled;
        return c.update(&stage);
    }
    match c.environment.restart(&state.resource, c.cancel) {
        Ok(proof) => {
            let valid = proof.running_verified && proof.container_id == state.resource.container_id;
            stage.restart = Some(proof);
            if valid {
                state.availability = Availability::Running;
            } else {
                stage
                    .errors
                    .push("restart did not verify same running container".into());
                state.availability = Availability::Unavailable;
                state.forward = false;
            }
        }
        Err(error) => {
            stage.errors.push(format!("restart: {error:#}"));
            state.availability = Availability::Unavailable;
            state.forward = false;
        }
    }
    if c.cancel.is_cancelled() {
        c.report.cancelled = true;
        state.availability = Availability::Unavailable;
        state.forward = false;
        stage.outcome = StageOutcome::Cancelled;
    } else {
        ScenarioContext::<E, F>::outcome(&mut stage);
    }
    c.update(&stage)
}
