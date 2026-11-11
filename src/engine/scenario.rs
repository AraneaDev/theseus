//! Scenario dispatch: creates the owned resource, then hands the shared stage
//! context to the scenario's module. Kept out of `stages` so the scenario
//! modules depend on `stages` without `stages` depending back on them.
use super::stages::{ScenarioContext, State};
use super::*;
use crate::checks;

pub(super) fn scenario<E: Environment, F: FnMut(EngineEvent)>(
    mut context: ScenarioContext<'_, E, F>,
) -> Result<()> {
    let c = &mut context;
    if c.cancel.is_cancelled() {
        let mut stage = StageReport::new("cancelled");
        stage.outcome = StageOutcome::Cancelled;
        stage.skip_reason = Some("cancelled before scenario creation".into());
        c.report.scenarios[c.index].stages.push(stage);
        c.report.cancelled = true;
        return Ok(());
    }
    let resource =
        c.environment
            .create(&c.report.run_id, &c.report.scenarios[c.index].id, c.recipe)?;
    c.report.scenarios[c.index].container_id = Some(resource.container_id.clone());
    c.journal.resources.push(resource.clone());
    c.store.save_journal(c.journal)?;
    c.save(EventKind::ScenarioStarted, None)?;
    c.environment.setup(&resource, c.recipe, c.cancel)?;
    let mut state = State::new(resource);
    let mut stage = StageReport::new("baseline");
    c.capture(&mut state, &mut stage);
    state.baseline = stage.snapshots.clone();
    c.finish(stage)?;
    let kind = c.report.scenarios[c.index].scenario;
    match kind {
        ScenarioKind::PluginLifecycle => super::plugin::run(c, &mut state)?,
        ScenarioKind::Downgrade => super::downgrade::run(c, &mut state)?,
        ScenarioKind::Rollback => super::rollback::run(c, &mut state)?,
        ScenarioKind::InterruptedRollback => super::interrupted_rollback::run(c, &mut state)?,
        ScenarioKind::InterruptedInstall | ScenarioKind::InterruptedUpgrade => {
            super::interruption::run(c, &mut state, kind)?
        }
        ScenarioKind::Clean | ScenarioKind::Upgrade | ScenarioKind::Reinstall => {
            super::common::run_common(
                &mut LegacyLifecycleBackend {
                    context: c,
                    state: &mut state,
                },
                kind,
            )?;
        }
    }
    if !matches!(
        kind,
        ScenarioKind::Clean | ScenarioKind::Upgrade | ScenarioKind::Reinstall
    ) {
        c.step(&mut state, StepName::Uninstall)?;
        c.checkpoint(&mut state, Checkpoint::Uninstalled, false)?;
    }
    let mut final_stage = StageReport::new("final");
    c.capture(&mut state, &mut final_stage);
    let final_state = final_stage.snapshots.clone();
    c.finish(final_stage)?;
    let profile = &c.recipe.recipe.profiles[&c.report.scenarios[c.index].profile];
    c.report.scenarios[c.index].findings =
        checks::review_findings(&state.baseline, &final_state, &profile.checks);
    Ok(())
}
/// Legacy bridge leaves the specialized Docker state and its safety semantics intact.
struct LegacyLifecycleBackend<'a, 'b, E, F> {
    context: &'a mut ScenarioContext<'b, E, F>,
    state: &'a mut State,
}
impl<E, F> super::common::sealed::Sealed for LegacyLifecycleBackend<'_, '_, E, F> {}
impl<E: Environment, F: FnMut(EngineEvent)> super::common::LifecycleBackend
    for LegacyLifecycleBackend<'_, '_, E, F>
{
    fn step(&mut self, step: StepName, name: &str) -> Result<()> {
        self.context.step_named(self.state, step, name)
    }
    fn checkpoint(&mut self, checkpoint: Checkpoint, configured: bool) -> Result<()> {
        self.context.checkpoint(self.state, checkpoint, configured)
    }
    fn before_reinstall(&mut self) -> Result<()> {
        let mut reference = StageReport::new("before_reinstall");
        self.context.capture(self.state, &mut reference);
        self.state.before_reinstall = Some(reference.snapshots.clone());
        self.context.finish(reference)
    }
}
