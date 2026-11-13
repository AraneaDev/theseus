//! Service hooks around the one ordinary lifecycle sequence.
use super::{ValidatedIntegrationRecipe, model::*};
use crate::{
    engine::{
        common::{self, LifecycleBackend, sealed},
        domain::StoredRun,
    },
    model::{CheckOutcome, Checkpoint, ScenarioKind, StepName},
    native::{adapter::NativeDriver, model::Disposition},
};
use anyhow::{Result, bail, ensure};

use std::time::Instant;
use theseus_native_protocol::v2;

mod compose;
mod control;
mod readiness;
mod service;
use compose::{
    OrdinaryIntegrationHooks, composed_before_reinstall, composed_checkpoint, composed_step,
};
pub struct IntegrationDriver<'a, 'b, 's> {
    core: &'a mut NativeDriver<'b, 's>,
    recipe: &'a ValidatedIntegrationRecipe,
    scenario: ScenarioKind,
    sequence: u32,
    last_observation: Option<v2::ServiceObservation>,
    first_install: bool,
    after_reinstall: bool,
    activation_action: Option<ActionReference>,
    activation_cause: ActivationCause,
    pending_control_observation: Option<String>,
    started: Instant,
    desktop: Option<super::desktop::DesktopHooks>,
}
impl<'a, 'b, 's> IntegrationDriver<'a, 'b, 's> {
    pub(crate) fn combined(
        core: &'a mut NativeDriver<'b, 's>,
        recipe: &'a ValidatedIntegrationRecipe,
        header: v2::broker::IntegrationBrokerHeader,
        scenario: ScenarioKind,
    ) -> Result<Self> {
        let hooks =
            super::desktop::DesktopDriver::new_for_scenario(&mut *core, recipe, header, scenario)?
                .into_hooks();
        let mut driver = Self::new(core, recipe);
        driver.desktop = Some(hooks);
        Ok(driver)
    }
    fn with_desktop(
        &mut self,
        f: impl FnOnce(&mut super::desktop::DesktopDriver<'_, 'b, 's>) -> Result<()>,
    ) -> Result<()> {
        let Some(hooks) = self.desktop.take() else {
            return Ok(());
        };
        let mut desktop = super::desktop::DesktopDriver::resume(&mut *self.core, hooks);
        let result = f(&mut desktop);
        self.desktop = Some(desktop.into_hooks());
        result
    }
    pub fn new(core: &'a mut NativeDriver<'b, 's>, recipe: &'a ValidatedIntegrationRecipe) -> Self {
        Self {
            core,
            recipe,
            scenario: ScenarioKind::Clean,
            sequence: 0,
            last_observation: None,
            first_install: true,
            after_reinstall: false,
            activation_action: None,
            activation_cause: ActivationCause::Sut,
            pending_control_observation: None,
            started: Instant::now(),
            desktop: None,
        }
    }
    pub fn run(&mut self, scenario: ScenarioKind) -> Result<()> {
        ensure!(
            self.recipe.recipe.integrations.desktop.is_some() == self.desktop.is_some(),
            "desktop integration requires exact composed session hooks"
        );
        self.scenario = scenario;
        self.core.prepare(scenario)?;
        self.inspect(IntegrationPhase::Baseline, None)?;
        ensure!(
            self.last_observation
                .as_ref()
                .is_some_and(|o| !o.registration_present && o.complete),
            "service baseline is not complete registration absence"
        );
        self.publish_binding(BindingBasis::VacantRegistration, None)?;
        self.with_desktop(|d| d.prepare_hooks(scenario))?;
        let result = common::run_common(self, scenario);
        if self.desktop.is_some() {
            self.with_desktop(|d| d.finish_hooks(result))
        } else {
            result
        }
    }
    fn evidence_mut(&mut self) -> Result<&mut IntegrationScenarioEvidence> {
        let ownership = &self.core.resource.resource().ownership_id;
        let StoredRun::NativeIntegration(report) = self.core.projector.report_mut() else {
            bail!("integration driver requires schema2 report")
        };
        report
            .integrations
            .iter_mut()
            .find(|s| &s.ownership_id == ownership)
            .ok_or_else(|| anyhow::anyhow!("integration scenario missing"))
    }
    fn service(&self) -> Result<&ServiceRequirement> {
        self.recipe
            .recipe
            .integrations
            .service
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("service requirement missing"))
    }
    fn sut_operation(&self) -> Option<String> {
        let StoredRun::NativeIntegration(report) = self.core.projector.report() else {
            return None;
        };
        report
            .catalog
            .scenarios
            .iter()
            .find(|s| s.ownership_id == self.core.resource.resource().ownership_id)?
            .stages
            .iter()
            .rev()
            .find(|s| s.step.is_some())?
            .commands
            .first()
            .map(|c| c.operation_id.clone())
    }
}
impl sealed::Sealed for IntegrationDriver<'_, '_, '_> {}
impl IntegrationDriver<'_, '_, '_> {
    fn before_service_step(&mut self, step: StepName) -> Result<()> {
        let forward = self.forward_hooks();
        if !integration_step_active(forward, step) {
            return Ok(());
        }
        if !forward && step == StepName::Uninstall {
            self.inspect_deferred_cleanup_authority()?;
        }
        let boundary = match step {
            StepName::Upgrade => Some((
                "upgrade",
                self.service()?.controls.upgrade_precondition,
                IntegrationPhase::Configured,
                ArtifactRole::Base,
            )),
            StepName::Reinstall => Some((
                "reinstall",
                self.service()?.controls.reinstall_precondition,
                IntegrationPhase::ExerciseBeforeReinstall,
                ArtifactRole::Target,
            )),
            StepName::Uninstall => Some((
                "uninstall",
                self.service()?.controls.uninstall_precondition,
                IntegrationPhase::BeforeUninstall,
                ArtifactRole::Target,
            )),
            _ => None,
        };
        if forward && let Some((name, precondition, phase, artifact)) = boundary {
            self.observe_runtime_precondition(name, precondition, phase, artifact)?;
        }
        if matches!(
            step,
            StepName::Configure | StepName::Upgrade | StepName::Reinstall
        ) {
            self.activation_action = None;
        }
        Ok(())
    }
    fn after_service_step(&mut self, step: StepName) -> Result<()> {
        if !integration_step_active(self.core.forward_active(), step) {
            return Ok(());
        }
        if matches!(step, StepName::Upgrade | StepName::Reinstall)
            || (step == StepName::Configure
                && self.service()?.controls.configured_activation == ActivationCause::Sut)
        {
            self.activation_cause = ActivationCause::Sut;
            self.activation_action = self.current_sut_action();
        }
        if step == StepName::Reinstall {
            self.after_reinstall = true;
        }
        if step == StepName::Exercise {
            let phase = exercise_phase(self.scenario, self.after_reinstall);
            self.inspect(
                phase,
                Some(DefinitionKey {
                    role: DefinitionRole::Configured,
                    artifact: ArtifactRole::Target,
                }),
            )?;
            self.publish_binding(
                BindingBasis::ExclusiveOwnedRegistration,
                Some(DefinitionKey {
                    role: DefinitionRole::Configured,
                    artifact: ArtifactRole::Target,
                }),
            )?;
            self.probe_readiness(
                phase,
                DefinitionKey {
                    role: DefinitionRole::Configured,
                    artifact: ArtifactRole::Target,
                },
            )?;
        }
        Ok(())
    }
    fn after_service_checkpoint(&mut self, checkpoint: Checkpoint, configured: bool) -> Result<()> {
        if !self.core.forward_active() && checkpoint != Checkpoint::Uninstalled {
            return Ok(());
        }
        let phase = match checkpoint {
            Checkpoint::BaseInstalled => IntegrationPhase::BaseInstalled,
            Checkpoint::TargetInstalled if self.first_install => {
                IntegrationPhase::TargetInstalledInitial
            }
            Checkpoint::TargetInstalled => IntegrationPhase::TargetInstalledAfterUpgrade,
            Checkpoint::Configured => IntegrationPhase::Configured,
            Checkpoint::Reinstalled => IntegrationPhase::Reinstalled,
            Checkpoint::Uninstalled => IntegrationPhase::Uninstalled,
            _ => bail!("unsupported integration checkpoint"),
        };
        let definition = checkpoint_definition(self.scenario, checkpoint);
        let activation_checkpoint = configured
            || matches!(
                phase,
                IntegrationPhase::TargetInstalledAfterUpgrade | IntegrationPhase::Reinstalled
            );
        if configured
            && self.service()?.controls.configured_activation == ActivationCause::RehearsalStart
        {
            self.inspect_configuration_only(phase, definition.clone())?;
            self.publish_binding(BindingBasis::ExclusiveOwnedRegistration, definition.clone())?;
            self.control(ServiceAction::Start)?;
        }
        self.inspect(phase, definition.clone())?;
        if definition.is_some() {
            self.publish_binding(BindingBasis::ExclusiveOwnedRegistration, definition.clone())?;
        }
        if activation_checkpoint {
            self.probe_readiness(
                phase,
                definition.ok_or_else(|| anyhow::anyhow!("configured definition missing"))?,
            )?;
        }
        self.first_install = false;
        if phase == IntegrationPhase::Uninstalled {
            let absent = self
                .last_observation
                .as_ref()
                .is_some_and(|o| o.complete && !o.registration_present && !o.running);
            let complete = self.last_observation.as_ref().is_some_and(|o| o.complete);
            self.evidence_mut()?.dispositions.registration = if !complete {
                Disposition::Unknown {
                    reason: "SUT removal observation incomplete".into(),
                }
            } else if absent {
                Disposition::Complete {
                    receipt: "SUT removed original registration before provider disposal".into(),
                }
            } else {
                Disposition::Failed {
                    reason: "SUT left original registration or runtime".into(),
                }
            };
            self.core.projector.save_report()?;
            self.publish_binding(BindingBasis::VacantRegistration, None)?;
        }
        Ok(())
    }
}
impl OrdinaryIntegrationHooks for IntegrationDriver<'_, '_, '_> {
    fn forward_hooks(&self) -> bool {
        self.core.forward_active() && self.desktop.as_ref().is_none_or(|d| d.forward_active())
    }
    fn service_before(&mut self, step: StepName) -> Result<()> {
        self.before_service_step(step)
    }
    fn desktop_before(&mut self, step: StepName) -> Result<()> {
        self.with_desktop(|d| d.before_step_hook(step))
    }
    fn core_step(&mut self, step: StepName, name: &str) -> Result<()> {
        self.core.step(step, name)
    }
    fn service_after(&mut self, step: StepName) -> Result<()> {
        self.after_service_step(step)
    }
    fn desktop_after(&mut self, step: StepName) -> Result<()> {
        self.with_desktop(|d| d.after_step_hook(step))
    }
    fn core_checkpoint(&mut self, checkpoint: Checkpoint, configured: bool) -> Result<()> {
        self.core.checkpoint(checkpoint, configured)
    }
    fn service_checkpoint(&mut self, checkpoint: Checkpoint, configured: bool) -> Result<()> {
        self.after_service_checkpoint(checkpoint, configured)
    }
    fn desktop_checkpoint(&mut self, checkpoint: Checkpoint, configured: bool) -> Result<()> {
        self.with_desktop(|d| d.checkpoint_hook(checkpoint, configured))
    }
    fn core_before_reinstall(&mut self) -> Result<()> {
        self.core.before_reinstall()
    }
}
impl LifecycleBackend for IntegrationDriver<'_, '_, '_> {
    fn step(&mut self, step: StepName, name: &str) -> Result<()> {
        composed_step(self, step, name)
    }
    fn checkpoint(&mut self, checkpoint: Checkpoint, configured: bool) -> Result<()> {
        composed_checkpoint(self, checkpoint, configured)
    }
    fn before_reinstall(&mut self) -> Result<()> {
        composed_before_reinstall(self)
    }
}
impl IntegrationDriver<'_, '_, '_> {
    fn current_sut_action(&self) -> Option<ActionReference> {
        let StoredRun::NativeIntegration(report) = self.core.projector.report() else {
            return None;
        };
        report
            .catalog
            .scenarios
            .iter()
            .find(|s| s.ownership_id == self.core.resource.resource().ownership_id)?
            .stages
            .iter()
            .rev()
            .find(|s| s.step.is_some())?
            .commands
            .first()
            .filter(|c| {
                c.failure.is_none() && c.status.as_ref() == Some(&c.command.expected_status)
            })
            .map(|c| ActionReference {
                operation: c.operation_id.clone(),
                request_digest: c.request_digest.clone(),
            })
    }
}

fn integration_step_active(forward: bool, step: StepName) -> bool {
    forward || step == StepName::Uninstall
}
pub(crate) fn exercise_phase(scenario: ScenarioKind, after_reinstall: bool) -> IntegrationPhase {
    if scenario == ScenarioKind::Reinstall {
        if after_reinstall {
            IntegrationPhase::ExerciseAfterReinstall
        } else {
            IntegrationPhase::ExerciseBeforeReinstall
        }
    } else {
        IntegrationPhase::ExerciseFinal
    }
}
fn checkpoint_definition(scenario: ScenarioKind, checkpoint: Checkpoint) -> Option<DefinitionKey> {
    let role = match checkpoint {
        Checkpoint::Uninstalled => return None,
        Checkpoint::BaseInstalled => DefinitionRole::Base,
        Checkpoint::Configured | Checkpoint::Reinstalled => DefinitionRole::Configured,
        Checkpoint::TargetInstalled if scenario == ScenarioKind::Upgrade => {
            DefinitionRole::Configured
        }
        _ => DefinitionRole::Target,
    };
    let artifact = if scenario == ScenarioKind::Upgrade
        && matches!(
            checkpoint,
            Checkpoint::BaseInstalled | Checkpoint::Configured
        ) {
        ArtifactRole::Base
    } else {
        ArtifactRole::Target
    };
    Some(DefinitionKey { role, artifact })
}
fn role_str(role: DefinitionRole) -> &'static str {
    match role {
        DefinitionRole::Base => "base",
        DefinitionRole::Configured => "configured",
        DefinitionRole::Target => "target",
    }
}
fn artifact_str(artifact: ArtifactRole) -> &'static str {
    match artifact {
        ArtifactRole::Base => "base",
        ArtifactRole::Target => "target",
    }
}
fn runtime_precondition_outcome(
    complete: bool,
    running: bool,
    precondition: RuntimePrecondition,
) -> CheckOutcome {
    if !complete {
        CheckOutcome::Incomplete
    } else if running == (precondition == RuntimePrecondition::Running) {
        CheckOutcome::Passed
    } else {
        CheckOutcome::Failed
    }
}
#[cfg(test)]
mod transition_sequence_tests {
    use super::*;
    #[test]
    fn model_accepts_only_declared_initial_rehearsal_runtime_with_original_action_reference() {
        let mut report = crate::native::integration::view::tests::report();
        let service = report.requirements.service.as_mut().unwrap();
        let required = &mut service.checks[0];
        required.phase = IntegrationPhase::Configured;
        required.role = Some(DefinitionRole::Configured);
        required.artifact = Some(ArtifactRole::Target);
        required.predicate = ServicePredicate::Running;
        report.compiled_requirements_digest = report.requirements.digest().unwrap();
        let integration = &mut report.integrations[0];
        let check = &mut integration.checks[0];
        check.phase = IntegrationPhase::Configured;
        check.role = Some(DefinitionRole::Configured);
        check.artifact = Some(ArtifactRole::Target);
        check.predicate = IntegrationPredicate::Service(ServicePredicate::Running);
        check.origin = CheckOrigin::RehearsalControl;
        check.outcome = CheckOutcome::Passed;
        check.observation_ids = vec!["running-after-start".into()];
        integration.services[0].actions.push(ActionReference {
            operation: "c".repeat(32),
            request_digest: "d".repeat(64),
        });
        let slot = integration.services[0].slot.clone();
        integration.services[0]
            .observations
            .push(BoundServiceObservation {
                platform: None,
                readiness: None,
                binding: ObservationBinding {
                    id: "running-after-start".into(),
                    run_id: report.run_id.clone(),
                    scenario_id: integration.scenario_id.clone(),
                    ownership_id: integration.ownership_id.clone(),
                    lease: "a".repeat(32),
                    slot,
                    request_id: "inspect-after-start".into(),
                    action: Some("c".repeat(32)),
                    binding_digest: "b".repeat(64),
                    source_profile: super::super::PROFILE.into(),
                    source_version: "fixture-v1".into(),
                    phase: IntegrationPhase::Configured,
                    definition: Some(DefinitionKey {
                        role: DefinitionRole::Configured,
                        artifact: ArtifactRole::Target,
                    }),
                    sequence: 1,
                    started_ms: 0,
                    finished_ms: 1,
                    complete: true,
                },
                state: IntegrationProvenance::ManagerReported {
                    manager: "original-manager".into(),
                    value: ServiceState::Present {
                        registered: true,
                        configured: true,
                        running: true,
                        definition_digest: "e".repeat(64),
                        origin: "original-fixture".into(),
                    },
                },
            });
        report.validate().unwrap();
        let mut missing = report.clone();
        missing.integrations[0].services[0].actions.clear();
        assert!(missing.validate().is_err());
        let mut undeclared = report.clone();
        undeclared
            .requirements
            .service
            .as_mut()
            .unwrap()
            .controls
            .configured_activation = ActivationCause::Sut;
        undeclared.compiled_requirements_digest = undeclared.requirements.digest().unwrap();
        assert!(undeclared.validate().is_err());
        let mut registration = report.clone();
        registration.requirements.service.as_mut().unwrap().checks[0].predicate =
            ServicePredicate::Registration;
        registration.integrations[0].checks[0].predicate =
            IntegrationPredicate::Service(ServicePredicate::Registration);
        registration.compiled_requirements_digest = registration.requirements.digest().unwrap();
        assert!(
            registration.validate().is_err(),
            "registration cannot be relabeled rehearsal"
        );
    }
    #[test]
    fn failed_core_step_prevents_later_transition_hooks_but_retains_uninstall() {
        struct FailedConfigure {
            forward: bool,
            effects: Vec<StepName>,
        }
        impl sealed::Sealed for FailedConfigure {}
        impl LifecycleBackend for FailedConfigure {
            fn step(&mut self, step: StepName, _: &str) -> Result<()> {
                if !integration_step_active(self.forward, step) {
                    return Ok(());
                }
                self.effects.push(step);
                if step == StepName::Configure {
                    self.forward = false;
                }
                Ok(())
            }
            fn checkpoint(&mut self, _: Checkpoint, _: bool) -> Result<()> {
                Ok(())
            }
            fn before_reinstall(&mut self) -> Result<()> {
                Ok(())
            }
        }
        for scenario in [ScenarioKind::Upgrade, ScenarioKind::Reinstall] {
            let mut driver = FailedConfigure {
                forward: true,
                effects: vec![],
            };
            common::run_common(&mut driver, scenario).unwrap();
            assert_eq!(
                driver.effects,
                vec![
                    if scenario == ScenarioKind::Upgrade {
                        StepName::InstallBase
                    } else {
                        StepName::InstallTarget
                    },
                    StepName::Configure,
                    StepName::Uninstall
                ]
            );
        }
    }
    #[test]
    fn upgrade_keeps_configured_role_and_changes_reader_only_after_sut() {
        assert_eq!(
            checkpoint_definition(ScenarioKind::Upgrade, Checkpoint::Configured),
            Some(DefinitionKey {
                role: DefinitionRole::Configured,
                artifact: ArtifactRole::Base
            })
        );
        assert_eq!(
            checkpoint_definition(ScenarioKind::Upgrade, Checkpoint::TargetInstalled),
            Some(DefinitionKey {
                role: DefinitionRole::Configured,
                artifact: ArtifactRole::Target
            })
        );
    }
    #[test]
    fn reinstall_has_distinct_exercise_intervals_and_configured_reference() {
        assert_eq!(
            exercise_phase(ScenarioKind::Reinstall, false),
            IntegrationPhase::ExerciseBeforeReinstall
        );
        assert_eq!(
            exercise_phase(ScenarioKind::Reinstall, true),
            IntegrationPhase::ExerciseAfterReinstall
        );
        assert_eq!(
            checkpoint_definition(ScenarioKind::Reinstall, Checkpoint::Reinstalled),
            Some(DefinitionKey {
                role: DefinitionRole::Configured,
                artifact: ArtifactRole::Target
            })
        );
    }
    #[test]
    fn running_precondition_cannot_be_satisfied_by_harness_stop_or_unknown() {
        for precondition in [
            RuntimePrecondition::Running,
            RuntimePrecondition::HarnessStopped,
        ] {
            assert_eq!(
                runtime_precondition_outcome(false, true, precondition),
                CheckOutcome::Incomplete
            );
            assert_eq!(
                runtime_precondition_outcome(false, false, precondition),
                CheckOutcome::Incomplete
            );
        }
        assert_eq!(
            runtime_precondition_outcome(true, false, RuntimePrecondition::Running),
            CheckOutcome::Failed
        );
        assert_eq!(
            runtime_precondition_outcome(true, false, RuntimePrecondition::HarnessStopped),
            CheckOutcome::Passed
        );
        assert_eq!(
            runtime_precondition_outcome(true, true, RuntimePrecondition::Running),
            CheckOutcome::Passed
        );
    }
}

#[cfg(test)]
mod combined_hook_tests {
    use super::*;
    struct Hooks {
        trace: Vec<String>,
        active: bool,
        reject_configure: bool,
        close_stops_service: bool,
        service_running: bool,
        observed_running: Vec<bool>,
    }
    impl Hooks {
        fn mark(&mut self, what: &str, value: impl std::fmt::Display) {
            self.trace.push(format!("{what}:{value}"));
        }
    }
    impl OrdinaryIntegrationHooks for Hooks {
        fn forward_hooks(&self) -> bool {
            self.active
        }
        fn service_before(&mut self, step: StepName) -> Result<()> {
            self.mark("service-before", step);
            if self.close_stops_service && matches!(step, StepName::Upgrade | StepName::Reinstall) {
                self.observed_running.push(self.service_running);
                if runtime_precondition_outcome(
                    true,
                    self.service_running,
                    RuntimePrecondition::Running,
                ) != CheckOutcome::Passed
                {
                    self.active = false;
                }
            }
            Ok(())
        }
        fn desktop_before(&mut self, step: StepName) -> Result<()> {
            self.mark("desktop-before", step);
            if self.close_stops_service && matches!(step, StepName::Upgrade | StepName::Reinstall) {
                self.service_running = false;
            }
            if self.reject_configure && step == StepName::Configure {
                self.active = false;
            }
            Ok(())
        }
        fn core_step(&mut self, step: StepName, _: &str) -> Result<()> {
            self.mark("sut-original-lease", step);
            Ok(())
        }
        fn service_after(&mut self, step: StepName) -> Result<()> {
            self.mark("service-after", step);
            Ok(())
        }
        fn desktop_after(&mut self, step: StepName) -> Result<()> {
            self.mark("desktop-after", step);
            Ok(())
        }
        fn core_checkpoint(&mut self, checkpoint: Checkpoint, _: bool) -> Result<()> {
            self.mark("core-check", checkpoint);
            Ok(())
        }
        fn service_checkpoint(&mut self, checkpoint: Checkpoint, _: bool) -> Result<()> {
            self.mark("service-check", checkpoint);
            Ok(())
        }
        fn desktop_checkpoint(&mut self, checkpoint: Checkpoint, _: bool) -> Result<()> {
            self.mark("desktop-check", checkpoint);
            Ok(())
        }
        fn core_before_reinstall(&mut self) -> Result<()> {
            self.trace.push("original-data-reference".into());
            Ok(())
        }
    }
    impl sealed::Sealed for Hooks {}
    impl LifecycleBackend for Hooks {
        fn step(&mut self, step: StepName, name: &str) -> Result<()> {
            composed_step(self, step, name)
        }
        fn checkpoint(&mut self, checkpoint: Checkpoint, configured: bool) -> Result<()> {
            composed_checkpoint(self, checkpoint, configured)
        }
        fn before_reinstall(&mut self) -> Result<()> {
            composed_before_reinstall(self)
        }
    }
    #[test]
    fn service_and_desktop_share_one_ordinary_sequence_and_original_lease() {
        for (scenario, expected) in [
            (
                ScenarioKind::Clean,
                vec!["install_target", "configure", "exercise", "uninstall"],
            ),
            (
                ScenarioKind::Upgrade,
                vec![
                    "install_base",
                    "configure",
                    "upgrade",
                    "exercise",
                    "uninstall",
                ],
            ),
            (
                ScenarioKind::Reinstall,
                vec![
                    "install_target",
                    "configure",
                    "exercise",
                    "reinstall",
                    "exercise",
                    "uninstall",
                ],
            ),
        ] {
            let mut hooks = Hooks {
                trace: vec![],
                active: true,
                reject_configure: false,
                close_stops_service: false,
                service_running: true,
                observed_running: vec![],
            };
            common::run_common(&mut hooks, scenario).unwrap();
            let actual = hooks
                .trace
                .iter()
                .filter_map(|s| s.strip_prefix("sut-original-lease:"))
                .collect::<Vec<_>>();
            assert_eq!(actual, expected);
            for (index, item) in hooks
                .trace
                .iter()
                .enumerate()
                .filter(|(_, s)| s.starts_with("sut-original-lease:"))
            {
                let step = item.strip_prefix("sut-original-lease:").unwrap();
                assert_eq!(
                    hooks.trace[index - 2..index + 3],
                    [
                        format!("desktop-before:{step}"),
                        format!("service-before:{step}"),
                        format!("sut-original-lease:{step}"),
                        format!("service-after:{step}"),
                        format!("desktop-after:{step}")
                    ]
                );
            }
            assert_eq!(
                hooks
                    .trace
                    .iter()
                    .filter(|s| s.as_str() == "original-data-reference")
                    .count(),
                usize::from(scenario == ScenarioKind::Reinstall)
            );
            if scenario == ScenarioKind::Reinstall {
                let reference = hooks
                    .trace
                    .iter()
                    .position(|s| s == "original-data-reference")
                    .unwrap();
                assert_eq!(
                    &hooks.trace[reference + 1..reference + 4],
                    [
                        "desktop-before:reinstall",
                        "service-before:reinstall",
                        "sut-original-lease:reinstall"
                    ]
                );
            }
        }
    }
    #[test]
    fn desktop_close_service_exit_is_observed_before_combined_mutation() {
        for step in [StepName::Upgrade, StepName::Reinstall] {
            let mut hooks = Hooks {
                trace: vec![],
                active: true,
                reject_configure: false,
                close_stops_service: true,
                service_running: true,
                observed_running: vec![],
            };
            composed_step(&mut hooks, step, "original combined SUT").unwrap();
            assert_eq!(
                hooks.observed_running,
                [false],
                "service precondition must observe the state after original desktop close"
            );
            assert!(
                !hooks
                    .trace
                    .iter()
                    .any(|s| s.starts_with("sut-original-lease:")),
                "GUI-induced service exit must block the declared Running SUT boundary"
            );
            assert!(!hooks.active);
        }
    }
    #[test]
    fn failed_desktop_before_skips_service_controls_but_keeps_uninstall() {
        let mut hooks = Hooks {
            trace: vec![],
            active: true,
            reject_configure: true,
            close_stops_service: false,
            service_running: true,
            observed_running: vec![],
        };
        composed_step(&mut hooks, StepName::Configure, "Configure").unwrap();
        assert!(
            !hooks.trace.iter().any(|s| s == "service-before:configure"),
            "failed desktop hook must not enter service controls"
        );
        composed_step(&mut hooks, StepName::Uninstall, "declared Uninstall").unwrap();
        assert!(hooks.trace.iter().any(|s| s == "service-before:uninstall"));
        assert!(
            hooks
                .trace
                .iter()
                .any(|s| s == "sut-original-lease:uninstall")
        );
    }
    #[test]
    fn combined_forward_failure_prevents_mutation_but_keeps_declared_sut_uninstall() {
        let mut hooks = Hooks {
            trace: vec![],
            active: true,
            reject_configure: true,
            close_stops_service: false,
            service_running: true,
            observed_running: vec![],
        };
        common::run_common(&mut hooks, ScenarioKind::Reinstall).unwrap();
        assert_eq!(
            hooks
                .trace
                .iter()
                .filter_map(|s| s.strip_prefix("sut-original-lease:"))
                .collect::<Vec<_>>(),
            ["install_target", "uninstall"]
        );
        assert!(!hooks.trace.iter().any(|s| s == "original-data-reference"));
        assert_eq!(
            &hooks.trace[hooks.trace.len() - 3..],
            [
                "core-check:uninstalled",
                "service-check:uninstalled",
                "desktop-check:uninstalled"
            ]
        );
    }
}
