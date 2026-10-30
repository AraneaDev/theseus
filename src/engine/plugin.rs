//! Plugin mutations share the engine's sticky prerequisites and owned cleanup.
use super::{
    stages::{ScenarioContext, State},
    *,
};
pub(super) fn run<E: Environment, F: FnMut(EngineEvent)>(
    c: &mut ScenarioContext<'_, E, F>,
    state: &mut State,
) -> Result<()> {
    c.step(state, StepName::InstallBase)?;
    c.checkpoint(state, Checkpoint::BaseInstalled, false)?;
    c.step(state, StepName::Upgrade)?;
    c.checkpoint(state, Checkpoint::TargetInstalled, false)?;
    c.step(state, StepName::DisablePlugin)?;
    c.checkpoint(state, Checkpoint::PluginDisabled, false)?;
    c.step(state, StepName::EnablePlugin)?;
    c.checkpoint(state, Checkpoint::PluginEnabled, false)?;
    Ok(())
}
