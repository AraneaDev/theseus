//! One ordinary lifecycle ordering, shared by the sealed Docker and native drivers.
use crate::model::{Checkpoint, ScenarioKind, StepName};
use anyhow::{Result, ensure};

pub(crate) mod sealed {
    pub(crate) trait Sealed {}
}
pub(crate) trait LifecycleBackend: sealed::Sealed {
    fn step(&mut self, step: StepName, name: &str) -> Result<()>;
    fn checkpoint(&mut self, checkpoint: Checkpoint, configured: bool) -> Result<()>;
    fn before_reinstall(&mut self) -> Result<()>;
}
/// Backend hooks retain their genuine evidence/state and use the same outcome projector.
/// This function is the only Clean/Upgrade/Reinstall scenario ordering.
pub(crate) fn run_common<D: LifecycleBackend>(driver: &mut D, kind: ScenarioKind) -> Result<()> {
    ensure!(
        matches!(
            kind,
            ScenarioKind::Clean | ScenarioKind::Upgrade | ScenarioKind::Reinstall
        ),
        "unsupported ordinary lifecycle scenario"
    );
    let (first, checkpoint) = if kind == ScenarioKind::Upgrade {
        (StepName::InstallBase, Checkpoint::BaseInstalled)
    } else {
        (StepName::InstallTarget, Checkpoint::TargetInstalled)
    };
    driver.step(first, &first.to_string())?;
    driver.checkpoint(checkpoint, false)?;
    driver.step(StepName::Configure, "configure")?;
    driver.checkpoint(Checkpoint::Configured, true)?;
    match kind {
        ScenarioKind::Upgrade => {
            driver.step(StepName::Upgrade, "upgrade")?;
            driver.checkpoint(Checkpoint::TargetInstalled, false)?;
            driver.step(StepName::Exercise, "exercise")?;
        }
        ScenarioKind::Reinstall => {
            driver.step(StepName::Exercise, "exercise_before_reinstall")?;
            driver.before_reinstall()?;
            driver.step(StepName::Reinstall, "reinstall")?;
            driver.checkpoint(Checkpoint::Reinstalled, false)?;
            driver.step(StepName::Exercise, "exercise_after_reinstall")?;
        }
        ScenarioKind::Clean => driver.step(StepName::Exercise, "exercise")?,
        _ => unreachable!("validated ordinary scenario"),
    }
    driver.step(StepName::Uninstall, "uninstall")?;
    driver.checkpoint(Checkpoint::Uninstalled, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Trace(Vec<String>);
    impl sealed::Sealed for Trace {}
    impl LifecycleBackend for Trace {
        fn step(&mut self, step: StepName, name: &str) -> Result<()> {
            self.0.push(format!("step:{step}:{name}"));
            Ok(())
        }
        fn checkpoint(&mut self, cp: Checkpoint, configured: bool) -> Result<()> {
            self.0.push(format!("check:{cp}:{configured}"));
            Ok(())
        }
        fn before_reinstall(&mut self) -> Result<()> {
            self.0.push("reference:before_reinstall".into());
            Ok(())
        }
    }
    #[test]
    fn shared_ordinary_order_and_reference() {
        let mut trace = Trace::default();
        run_common(&mut trace, ScenarioKind::Reinstall).unwrap();
        assert_eq!(
            trace.0,
            [
                "step:install_target:install_target",
                "check:target_installed:false",
                "step:configure:configure",
                "check:configured:true",
                "step:exercise:exercise_before_reinstall",
                "reference:before_reinstall",
                "step:reinstall:reinstall",
                "check:reinstalled:false",
                "step:exercise:exercise_after_reinstall",
                "step:uninstall:uninstall",
                "check:uninstalled:false"
            ]
        );
        let mut invalid = Trace::default();
        assert!(run_common(&mut invalid, ScenarioKind::Rollback).is_err());
        assert!(invalid.0.is_empty());
    }
}
