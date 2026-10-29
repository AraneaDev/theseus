//! Verified planned interruption followed by explicit restoration to known-good.
use super::{
    stages::{ScenarioContext, State},
    *,
};
pub(super) fn run<E: Environment, F: FnMut(EngineEvent)>(
    c: &mut ScenarioContext<'_, E, F>,
    state: &mut State,
) -> Result<()> {
    let policy = c.report.scenarios[c.index]
        .interrupted_rollback
        .clone()
        .expect("validated interrupted rollback policy");
    super::rollback::prepare(c, state, &policy.recovery)?;
    state.before_interrupt = Some(super::rollback::reference(c, state, "before_interrupt")?);
    super::interruption::stop(c, state, StepName::InterruptUpgrade)?;
    super::interruption::restart(c, state)?;
    c.checkpoint(state, Checkpoint::Interrupted, false)?;
    super::rollback::restore(c, state, &policy.recovery, Checkpoint::Interrupted)
}
