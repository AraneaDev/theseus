//! Generic step/checkpoint hook composition around the ordinary lifecycle core.
use crate::model::{Checkpoint, StepName};
use anyhow::Result;

// One step/checkpoint compositor around the original core; no scenario or owner loop.
pub(super) trait OrdinaryIntegrationHooks {
    fn forward_hooks(&self) -> bool;
    fn service_before(&mut self, step: StepName) -> Result<()>;
    fn desktop_before(&mut self, step: StepName) -> Result<()>;
    fn core_step(&mut self, step: StepName, name: &str) -> Result<()>;
    fn service_after(&mut self, step: StepName) -> Result<()>;
    fn desktop_after(&mut self, step: StepName) -> Result<()>;
    fn core_checkpoint(&mut self, checkpoint: Checkpoint, configured: bool) -> Result<()>;
    fn service_checkpoint(&mut self, checkpoint: Checkpoint, configured: bool) -> Result<()>;
    fn desktop_checkpoint(&mut self, checkpoint: Checkpoint, configured: bool) -> Result<()>;
    fn core_before_reinstall(&mut self) -> Result<()>;
}
pub(super) fn composed_step(
    h: &mut impl OrdinaryIntegrationHooks,
    step: StepName,
    name: &str,
) -> Result<()> {
    if !h.forward_hooks() && step != StepName::Uninstall {
        return Ok(());
    }
    // Desktop close can change a dependent service's runtime. Observe/control
    // the service only after close, immediately before the single SUT step.
    h.desktop_before(step)?;
    if !h.forward_hooks() && step != StepName::Uninstall {
        return Ok(());
    }
    h.service_before(step)?;
    if !h.forward_hooks() && step != StepName::Uninstall {
        return Ok(());
    }
    h.core_step(step, name)?;
    h.service_after(step)?;
    h.desktop_after(step)
}
pub(super) fn composed_checkpoint(
    h: &mut impl OrdinaryIntegrationHooks,
    checkpoint: Checkpoint,
    configured: bool,
) -> Result<()> {
    if !h.forward_hooks() && checkpoint != Checkpoint::Uninstalled {
        return Ok(());
    }
    h.core_checkpoint(checkpoint, configured)?;
    if !h.forward_hooks() && checkpoint != Checkpoint::Uninstalled {
        return Ok(());
    }
    h.service_checkpoint(checkpoint, configured)?;
    h.desktop_checkpoint(checkpoint, configured)
}
pub(super) fn composed_before_reinstall(h: &mut impl OrdinaryIntegrationHooks) -> Result<()> {
    if h.forward_hooks() {
        h.core_before_reinstall()?;
    }
    Ok(())
}
