use super::super::*;
use super::common::common_validate;
use crate::model::{CheckOutcome, RunOutcome, ScenarioKind};
use crate::native::model::*;
use anyhow::{Result, ensure};
use std::collections::{BTreeMap, BTreeSet};

impl NativeIntegrationReport {
    pub fn validate(&self) -> Result<()> {
        common_validate(
            (&self.kind, self.schema),
            "theseus_native_report",
            &self.profile,
            &self.run_id,
            [
                &self.recipe_digest,
                &self.core_recipe_digest,
                &self.core_manifest_digest,
                &self.integration_manifest_digest,
                &self.manifest_digest,
                &self.compiled_requirements_digest,
            ],
            &self.requirements,
            &self.selected_scenarios,
        )?;
        crate::native::model::validate_catalog_evidence(
            &self.catalog.scenarios,
            &self.catalog.identities,
            &self.catalog.provider_claims,
            &self.catalog.failures,
        )?;
        for scenario in &self.catalog.scenarios {
            if scenario.outcome == RunOutcome::Passed {
                ensure!(
                    [
                        &scenario.dispositions.local,
                        &scenario.dispositions.remote,
                        &scenario.dispositions.release,
                        &scenario.dispositions.reset
                    ]
                    .iter()
                    .all(|d| matches!(d, Disposition::Complete { .. })),
                    "passed catalog scenario has unproved core disposition"
                );
                ensure!(
                    !scenario.stages.is_empty()
                        && scenario.stages.iter().all(|stage| stage.outcome
                            == crate::model::StageOutcome::Passed
                            && stage.failures.is_empty()
                            && stage
                                .checks
                                .iter()
                                .all(|check| check.outcome == CheckOutcome::Passed)
                            && stage
                                .commands
                                .iter()
                                .all(|command| command.failure.is_none()
                                    && command.status.as_ref()
                                        == Some(&command.command.expected_status)
                                    && command.command.stdout_literal.as_ref().is_none_or(
                                        |expected| command.stdout_complete
                                            && command.stdout_utf8
                                            && &command.stdout == expected
                                    ))),
                    "passed catalog scenario contradicts core stage evidence"
                );
            }
        }
        ensure!(
            self.catalog.scenarios.len() <= self.selected_scenarios.len()
                && self.integrations.len() == self.catalog.scenarios.len()
                && (self.outcome != RunOutcome::Passed
                    || self.catalog.scenarios.len() == self.selected_scenarios.len()),
            "missing integration scenario evidence"
        );
        let mut observation_ids = BTreeSet::new();
        let mut scenario_ids = BTreeSet::new();
        for ((scenario, core), integration) in self
            .selected_scenarios
            .iter()
            .zip(&self.catalog.scenarios)
            .zip(&self.integrations)
        {
            ensure!(
                *scenario == core.scenario
                    && core.id == integration.scenario_id
                    && core.ownership_id == integration.ownership_id
                    && scenario_ids.insert(&core.id),
                "integration scenario binding mismatch"
            );
            for d in integration.dispositions.all() {
                d.validate()?;
            }
            ensure!(
                integration.services.len() <= usize::from(self.requirements.service.is_some())
                    && integration.desktops.len()
                        <= usize::from(self.requirements.desktop.is_some())
                    && (self.outcome != RunOutcome::Passed
                        || (integration.services.len()
                            == usize::from(self.requirements.service.is_some())
                            && integration.desktops.len()
                                == usize::from(self.requirements.desktop.is_some()))),
                "required integration resource evidence missing"
            );
            for service in &integration.services {
                ensure!(
                    self.requirements
                        .service
                        .as_ref()
                        .is_some_and(|r| r.resource == service.slot)
                        && service.observations.len() <= 64,
                    "undeclared service evidence"
                );
                for observation in &service.observations {
                    let b = &observation.binding;
                    b.validate()?;
                    if let Some(platform) = &observation.platform {
                        platform.validate()?;
                        let state = match &observation.state {
                            IntegrationProvenance::Observed { value, .. }
                            | IntegrationProvenance::ManagerReported { value, .. } => Some(value),
                            _ => None,
                        };
                        if b.complete {
                            use theseus_native_protocol::v2::{
                                LaunchdRuntimeState, ScmRegistrationState, ServicePlatformFacts,
                            };
                            match (platform, state) {
                                (
                                    ServicePlatformFacts::Scm {
                                        registration: ScmRegistrationState::Absent,
                                        ..
                                    },
                                    Some(ServiceState::Absent),
                                ) => {}
                                (
                                    ServicePlatformFacts::Scm {
                                        registration: ScmRegistrationState::Present,
                                        status: Some(status),
                                        ..
                                    },
                                    Some(ServiceState::Present { running, .. }),
                                ) => ensure!(
                                    *running == (status.state == 4),
                                    "SCM running projection contradicts native status"
                                ),
                                (
                                    ServicePlatformFacts::Scm {
                                        registration: ScmRegistrationState::SameNamePresent,
                                        ..
                                    },
                                    Some(ServiceState::Present { .. }),
                                ) => {}
                                (
                                    ServicePlatformFacts::Launchd {
                                        definition_present: false,
                                        loaded_registration_present: Some(false),
                                        output_truncated: false,
                                        ..
                                    },
                                    Some(ServiceState::Absent),
                                ) => {}
                                (
                                    ServicePlatformFacts::Launchd {
                                        definition_present: true,
                                        loaded_registration_present: Some(false),
                                        output_truncated: false,
                                        ..
                                    },
                                    Some(ServiceState::Present { running: false, .. }),
                                ) => {}
                                (
                                    ServicePlatformFacts::Launchd {
                                        runtime_state: Some(runtime),
                                        output_truncated: false,
                                        ..
                                    },
                                    Some(ServiceState::Present { running, .. }),
                                ) => ensure!(
                                    *running == (*runtime == LaunchdRuntimeState::Running),
                                    "launchd running projection contradicts qualified runtime"
                                ),
                                _ => anyhow::bail!(
                                    "complete platform observation contradicts native registration/runtime facts"
                                ),
                            }
                        }
                        if let theseus_native_protocol::v2::ServicePlatformFacts::Scm {
                            registration,
                            ..
                        } = platform
                        {
                            ensure!(!b.complete || !matches!(registration,theseus_native_protocol::v2::ScmRegistrationState::DeletePending|theseus_native_protocol::v2::ScmRegistrationState::Unknown),"pending SCM registration cannot be a complete observation");
                        }
                    }
                    if let Some(readiness) = &observation.readiness {
                        readiness.validate()?;
                        ensure!(
                            readiness.slot.as_str() == b.slot.as_str()
                                && readiness.resource_binding_digest == b.binding_digest
                                && b.action.as_ref() == Some(&readiness.operation),
                            "readiness report binding mismatch"
                        );
                    }
                    ensure!(
                        b.run_id == self.run_id
                            && b.scenario_id == core.id
                            && b.ownership_id == core.ownership_id
                            && b.slot == service.slot
                            && observation_ids.insert(&b.id),
                        "service observation binding mismatch"
                    );
                }
            }
            for desktop in &integration.desktops {
                ensure!(
                    self.requirements
                        .desktop
                        .as_ref()
                        .is_some_and(|d| d.resource == desktop.slot),
                    "desktop observation slot undeclared"
                );
                ensure!(
                    desktop.observations.len() <= 128,
                    "too many desktop observations"
                );
                let mut original_states = BTreeMap::new();
                for observation in &desktop.observations {
                    let b = &observation.binding;
                    b.validate()?;
                    ensure!(
                        matches!(
                            b.phase,
                            IntegrationPhase::Baseline | IntegrationPhase::Uninstalled
                        ) || (core.scenario == ScenarioKind::Clean && b.definition.is_none())
                            || observation.generation.is_some(),
                        "keyed desktop observation lacks original SUT generation"
                    );
                    if let Some(generation) = &observation.generation {
                        generation.validate()?;
                        ensure!(
                            generation.sut.lease == b.lease
                                && b.definition
                                    == Some(DefinitionKey::from_desktop_wire(
                                        generation.definition
                                    )),
                            "desktop generation changed original lease/definition"
                        );
                    }
                    ensure!(
                        b.run_id == self.run_id
                            && b.scenario_id == core.id
                            && b.ownership_id == core.ownership_id
                            && b.slot == desktop.slot
                            && observation_ids.insert(&b.id),
                        "desktop observation binding mismatch"
                    );
                    if let Some(state) = &observation.broker_state {
                        state.validate()?;
                        let requirement = self.requirements.desktop.as_ref().unwrap();
                        if let Some(native) = &state.native_lifetime {
                            ensure!(
                                requirement.modes == [DesktopMode::NativeActivation]
                                    && native.required == requirement.lifetime.native(),
                                "native observation exceeds selected mode/requirement"
                            );
                        }
                        if let theseus_native_protocol::v2::broker::BrokerEffect::Receipt {
                            receipt,
                        } = &state.effect
                        {
                            let native=receipt.evidence==theseus_native_protocol::v2::broker::DesktopEvidence::NativeActivation;
                            ensure!(
                                native == (requirement.modes == [DesktopMode::NativeActivation]),
                                "desktop receipt borrowed another selected mode"
                            );
                        }
                        if let Some(action) = &b.action
                            && let Some(previous) = original_states.insert(action, state)
                        {
                            validate_broker_progress(previous, state)?;
                        }
                        if let theseus_native_protocol::v2::broker::BrokerEffect::Receipt {
                            receipt,
                        } = &state.effect
                        {
                            ensure!(
                                observation.evidence == Some(receipt.evidence),
                                "desktop evidence label differs from original broker receipt"
                            );
                        }
                    }
                    if let Some(ready) = &observation.readiness {
                        ready.validate_desktop_fields()?;
                        ensure!(
                            b.action.as_ref() == Some(&ready.operation)
                                && b.binding_digest == ready.resource_binding_digest
                                && ready.slot.as_str() == b.slot.as_str(),
                            "desktop readiness original interval differs"
                        );
                    }
                }
            }
            crate::native::integration::owned_lifetime::validate_report_scenario(
                &self.requirements,
                integration,
            )?;
            if let Some(native) = &integration.native_lifetime {
                native.state.validate()?;
                let required = self.requirements.desktop.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("native lifetime without desktop requirements")
                })?;
                ensure!(
                    required.modes == [DesktopMode::NativeActivation]
                        && native.state.required == required.lifetime.native(),
                    "native lifetime exceeds selected mode/requirement"
                );
                ensure!(
                    integration
                        .desktops
                        .iter()
                        .filter(|d| d.slot == required.resource)
                        .any(|d| d.actions.contains(&native.original)
                            && d.observations.iter().any(|o| o.binding.action.as_ref()
                                == Some(&native.original.operation)
                                && o.broker_state
                                    .as_ref()
                                    .is_some_and(
                                        |s| s.native_lifetime.as_ref() == Some(&native.state)
                                    ))),
                    "native projection lacks exact original observed action"
                );
            }
            let mut check_ids = BTreeSet::new();
            for check in &integration.checks {
                text(&check.id, 128)?;
                text(&check.detail, 4096)?;
                check.slot.validate()?;
                ensure!(
                    check.scenario_id == core.id && check_ids.insert(&check.id),
                    "duplicate or wrong-scenario integration check"
                );
                if check.outcome == CheckOutcome::Passed {
                    ensure!(
                        !check.observation_ids.is_empty(),
                        "passed integration check has no observations"
                    );
                    for id in &check.observation_ids {
                        if let IntegrationPredicate::Service(predicate) = check.predicate {
                            let observation = integration
                                .services
                                .iter()
                                .flat_map(|s| &s.observations)
                                .find(|o| &o.binding.id == id)
                                .ok_or_else(|| {
                                    anyhow::anyhow!("service check observation missing")
                                })?;
                            let state = match &observation.state {
                                IntegrationProvenance::Observed { value, .. }
                                | IntegrationProvenance::ManagerReported { value, .. } => value,
                                _ => anyhow::bail!(
                                    "passed service predicate lacks available manager observation"
                                ),
                            };
                            let satisfied = match (predicate, state) {
                                (ServicePredicate::RegistrationAbsent, ServiceState::Absent) => {
                                    true
                                }
                                (
                                    ServicePredicate::Registration,
                                    ServiceState::Present { registered, .. },
                                ) => *registered,
                                (
                                    ServicePredicate::Configuration,
                                    ServiceState::Present {
                                        registered,
                                        configured,
                                        ..
                                    },
                                ) => *registered && *configured,
                                (
                                    ServicePredicate::Running,
                                    ServiceState::Present {
                                        registered,
                                        running,
                                        ..
                                    },
                                ) => *registered && *running,
                                (ServicePredicate::Stopped, ServiceState::Absent) => true,
                                (
                                    ServicePredicate::Stopped,
                                    ServiceState::Present { running, .. },
                                ) => !*running,
                                (ServicePredicate::ApplicationReady, _) => {
                                    observation.readiness.as_ref().is_some_and(|r| {
                                        r.state
                                            == theseus_native_protocol::v2::ReadinessState::Ready
                                            && r.authenticated_instance
                                            && r.initialized == Some(true)
                                            && r.response_digest.is_some()
                                            && r.resource_binding_digest
                                                == observation.binding.binding_digest
                                    })
                                }
                                _ => false,
                            };
                            ensure!(
                                satisfied,
                                "passed service predicate contradicts observed facts"
                            );
                        }
                        if !matches!(check.predicate, IntegrationPredicate::Service(_)) {
                            let observation = integration
                                .desktops
                                .iter()
                                .flat_map(|d| &d.observations)
                                .find(|o| &o.binding.id == id)
                                .ok_or_else(|| {
                                    anyhow::anyhow!("desktop predicate observation missing")
                                })?;
                            ensure!(
                                desktop_predicate_satisfied(observation, &check.predicate),
                                "passed desktop predicate contradicts original independent facts"
                            );
                        }
                        let b = integration
                            .services
                            .iter()
                            .flat_map(|s| &s.observations)
                            .map(|o| &o.binding)
                            .chain(
                                integration
                                    .desktops
                                    .iter()
                                    .flat_map(|s| &s.observations)
                                    .map(|o| &o.binding),
                            )
                            .find(|b| &b.id == id)
                            .ok_or_else(|| anyhow::anyhow!("check observation is absent"))?;
                        ensure!(
                            b.complete
                                && b.phase == check.phase
                                && b.slot == check.slot
                                && b.definition.as_ref().map(|k| k.role) == check.role
                                && b.definition.as_ref().map(|k| k.artifact) == check.artifact,
                            "check observation incomplete or wrong interval"
                        );
                    }
                }
            }
            if let Some(service) = &self.requirements.service {
                for required in service
                    .checks
                    .iter()
                    .filter(|c| c.scenarios.contains(scenario))
                {
                    let Some(check) = integration.checks.iter().find(|c| c.id == required.id)
                    else {
                        ensure!(
                            self.outcome != RunOutcome::Passed,
                            "required integration check missing"
                        );
                        continue;
                    };
                    let declared_rehearsal_runtime = check.origin == CheckOrigin::RehearsalControl
                        && service.controls.configured_activation
                            == ActivationCause::RehearsalStart
                        && matches!(
                            required.predicate,
                            ServicePredicate::Running | ServicePredicate::ApplicationReady
                        )
                        && (required.phase == IntegrationPhase::Configured
                            || (*scenario == ScenarioKind::Clean
                                && required.phase == IntegrationPhase::ExerciseFinal)
                            || (*scenario == ScenarioKind::Reinstall
                                && required.phase == IntegrationPhase::ExerciseBeforeReinstall));
                    if declared_rehearsal_runtime && check.outcome == CheckOutcome::Passed {
                        ensure!(
                            !check.observation_ids.is_empty()
                                && check.observation_ids.iter().all(|id| {
                                    integration
                                        .services
                                        .iter()
                                        .filter(|s| s.slot == service.resource)
                                        .any(|s| {
                                            s.observations
                                                .iter()
                                                .find(|o| &o.binding.id == id)
                                                .is_some_and(|observation| {
                                                    observation.binding.action.as_ref().is_some_and(
                                                        |operation| {
                                                            s.actions
                                                                .iter()
                                                                .any(|a| &a.operation == operation)
                                                        },
                                                    )
                                                })
                                        })
                                }),
                            "rehearsal runtime lacks original public Start reference"
                        );
                    }
                    ensure!(
                        check.phase == required.phase
                            && check.slot == service.resource
                            && check.role == required.role
                            && check.artifact == required.artifact
                            && check.predicate == IntegrationPredicate::Service(required.predicate)
                            && (check.origin == CheckOrigin::SutPostcondition
                                || declared_rehearsal_runtime),
                        "required integration check changed"
                    );
                }
            }
            if let Some(desktop) = &self.requirements.desktop {
                for (id, phase, predicate, origin) in
                    desktop_scenario_check_specs(desktop, core.scenario)
                {
                    if let Some(check) = integration.checks.iter().find(|c| c.id == id) {
                        ensure!(
                            check.phase == phase
                                && check.slot == desktop.resource
                                && check.predicate == predicate
                                && check.origin == origin
                                && ((core.scenario == ScenarioKind::Clean
                                    && check.role.is_none()
                                    && check.artifact.is_none())
                                    || desktop_phase_definition(core.scenario, phase)
                                        .is_some_and(|d| check.role == Some(d.role)
                                            && check.artifact == Some(d.artifact))
                                    || (desktop_phase_definition(core.scenario, phase).is_none()
                                        && check.role.is_none()
                                        && check.artifact.is_none())),
                            "required desktop check changed"
                        );
                    } else {
                        ensure!(
                            self.outcome != RunOutcome::Passed,
                            "required desktop predicate missing"
                        );
                    }
                }
            }
            if self.outcome == RunOutcome::Passed {
                ensure!(
                    core.outcome == RunOutcome::Passed
                        && integration
                            .checks
                            .iter()
                            .all(|c| c.outcome == CheckOutcome::Passed)
                        && integration.complete(),
                    "integration success lacks proved required evidence"
                );
            }
        }
        if self.outcome == RunOutcome::Passed {
            self.requirements
                .validate_required(&self.selected_scenarios)?;
        }
        for receipt in &self.catalog.provider_claims {
            receipt.validate()?;
            ensure!(
                receipt.manifest_digest == self.manifest_digest,
                "provider receipt full manifest mismatch"
            );
        }
        Ok(())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value: Self = decode(bytes)?;
        value.validate()?;
        Ok(value)
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(serde_json::to_vec_pretty(self)?)
    }
}
