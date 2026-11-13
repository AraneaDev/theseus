use super::super::*;
use crate::model::{CheckOutcome, ScenarioKind};
use crate::native::model::*;
use anyhow::{Result, ensure};

pub fn validate_pair(
    report: &NativeIntegrationReport,
    journal: &NativeIntegrationJournal,
) -> Result<()> {
    report.validate()?;
    journal.validate()?;
    ensure!(
        report.run_id == journal.run_id
            && report.recipe_digest == journal.recipe_digest
            && report.core_recipe_digest == journal.core_recipe_digest
            && report.core_manifest_digest == journal.core_manifest_digest
            && report.integration_manifest_digest == journal.integration_manifest_digest
            && report.manifest_digest == journal.manifest_digest
            && report.requirements == journal.requirements
            && report.compiled_requirements_digest == journal.compiled_requirements_digest
            && report.selected_scenarios == journal.selected_scenarios,
        "integration report/journal pair mismatch"
    );
    validate_recovery_report_progress(report, journal)?;
    validate_catalog_operations(report, journal)?;
    validate_desktop_observations(report, journal)?;
    validate_rehearsal_starts(report, journal)?;
    validate_sut_readiness(report, journal)?;
    validate_public_readiness(report, journal)
}

fn validate_recovery_report_progress(
    report: &NativeIntegrationReport,
    journal: &NativeIntegrationJournal,
) -> Result<()> {
    for recovery in &journal.integration_recoveries {
        for observation in &recovery.broker_observations {
            let action = journal
                .integration_actions
                .iter()
                .find(|a| {
                    a.ownership_id == recovery.ownership_id
                        && a.operation == observation.original.operation
                        && a.request_digest == observation.original.request_digest
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("broker recovery lacks original action for report comparison")
                })?;
            validate_original_broker_report_progress(report, action, &observation.state)?;
        }
    }
    Ok(())
}

fn validate_catalog_operations(
    report: &NativeIntegrationReport,
    journal: &NativeIntegrationJournal,
) -> Result<()> {
    for scenario in &report.catalog.scenarios {
        for command in scenario.stages.iter().flat_map(|s| &s.commands) {
            ensure!(
                journal
                    .operations
                    .iter()
                    .any(|op| op.ownership_id == scenario.ownership_id
                        && op.operation_id == command.operation_id
                        && op.command == command.command
                        && op.request_digest == command.request_digest),
                "catalog evidence lacks exact original published operation"
            );
        }
    }
    Ok(())
}

fn validate_desktop_observations(
    report: &NativeIntegrationReport,
    journal: &NativeIntegrationJournal,
) -> Result<()> {
    for integration in &report.integrations {
        crate::native::integration::owned_lifetime::validate_pair_projection(integration, journal)?;
        for desktop in &integration.desktops {
            for observation in &desktop.observations {
                validate_desktop_observation_origin(report, journal, observation)?;
            }
        }
    }
    Ok(())
}

fn validate_desktop_observation_origin(
    report: &NativeIntegrationReport,
    journal: &NativeIntegrationJournal,
    observation: &BoundDesktopObservation,
) -> Result<()> {
    validate_desktop_observation_generation(report, journal, observation)?;
    let binding = &observation.binding;
    ensure!(journal.ownership.iter().any(|owner|owner.ownership_id==binding.ownership_id && owner.scenario_id==binding.scenario_id && matches!(&owner.ownership,NativeOwnership::Acquired {lease,..} if lease==&binding.lease)),"desktop observation original helper lease missing");
    if observation.broker_state.is_some() || observation.readiness.is_some() {
        let action = journal
            .integration_actions
            .iter()
            .find(|a| {
                a.ownership_id == binding.ownership_id
                    && a.lease == binding.lease
                    && a.slot == binding.slot
                    && binding.action.as_ref() == Some(&a.operation)
            })
            .ok_or_else(|| {
                anyhow::anyhow!("desktop observation lacks original published activation")
            })?;
        let IntegrationRequest::Desktop { request, .. } = &action.request else {
            anyhow::bail!("desktop observation borrowed a service operation")
        };
        ensure!(
            matches!(
                request.action,
                theseus_native_protocol::v2::broker::DesktopAction::Activate { .. }
            ) && request.prior_binding_digest == binding.binding_digest,
            "desktop observation changed original activation context"
        );
        validate_desktop_observation_lifetimes(
            report,
            journal,
            observation,
            binding,
            action,
            request,
        )?;
        if let Some(ready) = &observation.readiness {
            validate_desktop_observation_readiness(journal, binding, request, ready)?;
        }
    }
    Ok(())
}

fn validate_desktop_observation_lifetimes(
    report: &NativeIntegrationReport,
    journal: &NativeIntegrationJournal,
    observation: &BoundDesktopObservation,
    binding: &ObservationBinding,
    action: &IntegrationActionRecord,
    request: &theseus_native_protocol::v2::broker::DesktopRequest,
) -> Result<()> {
    if let Some(state) = &observation.broker_state
        && (request.owned_lifetime_definition().is_some() || state.owned_lifetime.is_some())
    {
        let broker = journal
            .broker_bindings
            .iter()
            .find(|b| {
                b.ownership_id == binding.ownership_id
                    && b.helper_lease == binding.lease
                    && b.broker_lease.as_ref() == Some(&request.broker_lease)
            })
            .ok_or_else(|| anyhow::anyhow!("owned lifetime original broker mapping missing"))?;
        crate::native::integration::owned_lifetime::validate_original_observation(
            request,
            state,
            report.requirements.desktop.as_ref(),
            &broker.original.expected_header.os,
        )?;
        crate::native::integration::owned_lifetime::validate_report_effect(action, state)?;
    }
    if let Some(native) = observation
        .broker_state
        .as_ref()
        .and_then(|s| s.native_lifetime.as_ref())
    {
        let broker = journal
            .broker_bindings
            .iter()
            .find(|b| {
                b.ownership_id == binding.ownership_id
                    && b.helper_lease == binding.lease
                    && b.broker_lease.as_ref() == Some(&request.broker_lease)
            })
            .ok_or_else(|| anyhow::anyhow!("native lifetime original broker mapping missing"))?;
        let required = report
            .requirements
            .desktop
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("native lifetime requirements missing"))?;
        ensure!(
            required.modes == [DesktopMode::NativeActivation]
                && native.required == required.lifetime.native()
                && matches!(
                    request.action,
                    theseus_native_protocol::v2::broker::DesktopAction::Activate {
                        mode: theseus_native_protocol::v2::broker::DesktopMode::NativeActivation,
                        ..
                    }
                ),
            "native lifetime borrowed another mode/requirement"
        );
        native.validate_platform(&broker.original.expected_header.os)?;
    }
    Ok(())
}

fn validate_desktop_observation_readiness(
    journal: &NativeIntegrationJournal,
    binding: &ObservationBinding,
    request: &theseus_native_protocol::v2::broker::DesktopRequest,
    ready: &theseus_native_protocol::v2::ReadinessObservation,
) -> Result<()> {
    let identity = request.identity()?;
    ready.validate_desktop(&identity)?;
    let broker = journal
        .broker_bindings
        .iter()
        .find(|b| {
            b.ownership_id == binding.ownership_id
                && b.helper_lease == binding.lease
                && b.broker_lease.as_ref() == Some(&identity.broker_lease)
        })
        .ok_or_else(|| anyhow::anyhow!("desktop readiness original broker mapping missing"))?;
    ready.validate_desktop_peer(&broker.original.expected_header.broker_principal)?;
    let probe = journal
        .readiness_probes
        .iter()
        .find(|p| {
            p.challenge.ownership_id == binding.ownership_id
                && p.challenge.slot == binding.slot
                && p.challenge.resource_binding_digest == binding.binding_digest
                && p.challenge.operation == identity.helper_operation
                && p.challenge.lease == identity.broker_lease
                && p.challenge
                    .digest()
                    .is_ok_and(|d| d == ready.challenge_digest)
                && p.expectation_digest == ready.expectation_digest
                && p.expectation.wire() == ready.expectation
                && p.observation_ids.contains(&binding.id)
        })
        .ok_or_else(|| anyhow::anyhow!("desktop readiness lacks exact original published probe"))?;
    if ready.state == theseus_native_protocol::v2::ReadinessState::Ready {
        ensure!(
            matches!(&probe.completion,Disposition::Complete {receipt} if Some(receipt)==ready.response_digest.as_ref()),
            "desktop Ready response lacks exact completed private receipt"
        );
    }
    Ok(())
}

fn validate_rehearsal_starts(
    report: &NativeIntegrationReport,
    journal: &NativeIntegrationJournal,
) -> Result<()> {
    // Public runtime predicates may name a declared rehearsal Start, but a
    // report reference cannot turn a Stop, another lease, or an unfinished
    // action into that accepted original cause.
    for integration in &report.integrations {
        let Some(service) = &report.requirements.service else {
            continue;
        };
        for check in integration.checks.iter().filter(|check| {
            check.origin == CheckOrigin::RehearsalControl
                && check.outcome == CheckOutcome::Passed
                && service
                    .checks
                    .iter()
                    .any(|required| required.id == check.id)
        }) {
            for id in &check.observation_ids {
                let evidence = integration
                    .services
                    .iter()
                    .find(|s| s.slot == check.slot)
                    .ok_or_else(|| anyhow::anyhow!("rehearsal service evidence missing"))?;
                let observation = evidence
                    .observations
                    .iter()
                    .find(|o| &o.binding.id == id)
                    .ok_or_else(|| anyhow::anyhow!("rehearsal observation missing"))?;
                ensure!(
                    evidence
                        .actions
                        .iter()
                        .any(|reference| observation.binding.action.as_ref()
                            == Some(&reference.operation)
                            && journal.integration_actions.iter().any(|action| action
                                .ownership_id
                                == integration.ownership_id
                                && action.lease == observation.binding.lease
                                && action.slot == observation.binding.slot
                                && action.operation == reference.operation
                                && action.request_digest == reference.request_digest
                                && matches!(
                                    action.request,
                                    IntegrationRequest::Service {
                                        control: ServiceAction::Start,
                                        ..
                                    }
                                )
                                && matches!(action.completion, Disposition::Complete { .. })
                                && matches!(
                                    action.terminal,
                                    Some(IntegrationOutcome::Complete { .. })
                                ))),
                    "rehearsal runtime lacks exact completed original Start"
                );
            }
        }
    }
    Ok(())
}

fn validate_sut_readiness(
    report: &NativeIntegrationReport,
    journal: &NativeIntegrationJournal,
) -> Result<()> {
    for probe in &journal.readiness_probes {
        let challenge = &probe.challenge;
        let Some(operation) = journal.operations.iter().find(|operation| {
            operation.ownership_id == challenge.ownership_id
                && operation.lease == challenge.lease
                && operation.operation_id == challenge.operation
                && operation.request_digest == challenge.action_digest
        }) else {
            continue;
        }; // The journal validator already proved exact original Start.
        let scenario = report
            .catalog
            .scenarios
            .iter()
            .find(|s| s.ownership_id == challenge.ownership_id)
            .ok_or_else(|| anyhow::anyhow!("SUT readiness original scenario missing"))?;
        let required_step = match (operation.command.program, scenario.scenario) {
            (CatalogProgram::Configure, _) => crate::model::StepName::Configure,
            (CatalogProgram::InstallTarget, ScenarioKind::Upgrade) => {
                crate::model::StepName::Upgrade
            }
            (CatalogProgram::InstallTarget, ScenarioKind::Reinstall) => {
                crate::model::StepName::Reinstall
            }
            _ => {
                anyhow::bail!("SUT readiness command is not this scenario's activation transition")
            }
        };
        let expected_artifact = if operation.command.program == CatalogProgram::Configure
            && scenario.scenario == ScenarioKind::Upgrade
        {
            ArtifactRole::Base
        } else {
            ArtifactRole::Target
        };
        ensure!(
            probe.expectation.role == DefinitionRole::Configured
                && probe.expectation.artifact == expected_artifact,
            "SUT readiness expectation differs from original activation phase"
        );
        ensure!(
            scenario
                .stages
                .iter()
                .filter(|s| s.step == Some(required_step))
                .flat_map(|s| &s.commands)
                .any(|command| command.operation_id == operation.operation_id
                    && command.request_digest == operation.request_digest
                    && command.command == operation.command
                    && command.failure.is_none()
                    && command.status.as_ref() == Some(&command.command.expected_status)
                    && matches!(
                        command.status,
                        Some(
                            NativeStatus::PosixExit { code: 0 }
                                | NativeStatus::WindowsExit { code: 0 }
                        )
                    )
                    && command
                        .command
                        .stdout_literal
                        .as_ref()
                        .is_none_or(|expected| command.stdout_complete
                            && command.stdout_utf8
                            && &command.stdout == expected)),
            "SUT readiness lacks exact successful original activation Run evidence"
        );
        validate_sut_readiness_observations(report, probe, operation, scenario)?;
    }
    Ok(())
}

fn validate_sut_readiness_observations(
    report: &NativeIntegrationReport,
    probe: &ReadinessProbeRecord,
    operation: &NativeOperation,
    scenario: &NativeScenario,
) -> Result<()> {
    let challenge = &probe.challenge;
    let integration = report
        .integrations
        .iter()
        .find(|s| s.ownership_id == challenge.ownership_id)
        .ok_or_else(|| anyhow::anyhow!("SUT readiness integration scenario missing"))?;
    for id in &probe.observation_ids {
        let Some(observation) = integration
            .services
            .iter()
            .filter(|s| s.slot == challenge.slot)
            .flat_map(|s| &s.observations)
            .find(|o| &o.binding.id == id)
        else {
            // The private completion barrier precedes the public report.
            // Missing historical bytes remain unavailable after a crash;
            // no required public predicate can pass without its observation.
            continue;
        };
        let phase = observation.binding.phase;
        let phase_matches = match (operation.command.program, scenario.scenario) {
            (CatalogProgram::Configure, ScenarioKind::Clean) => matches!(
                phase,
                IntegrationPhase::Configured | IntegrationPhase::ExerciseFinal
            ),
            (CatalogProgram::Configure, ScenarioKind::Upgrade) => {
                phase == IntegrationPhase::Configured
            }
            (CatalogProgram::Configure, ScenarioKind::Reinstall) => matches!(
                phase,
                IntegrationPhase::Configured | IntegrationPhase::ExerciseBeforeReinstall
            ),
            (CatalogProgram::InstallTarget, ScenarioKind::Upgrade) => matches!(
                phase,
                IntegrationPhase::TargetInstalledAfterUpgrade | IntegrationPhase::ExerciseFinal
            ),
            (CatalogProgram::InstallTarget, ScenarioKind::Reinstall) => matches!(
                phase,
                IntegrationPhase::Reinstalled | IntegrationPhase::ExerciseAfterReinstall
            ),
            _ => false,
        };
        ensure!(
            phase_matches,
            "SUT readiness observation phase differs from original activation interval"
        );
    }
    Ok(())
}

fn validate_public_readiness(
    report: &NativeIntegrationReport,
    journal: &NativeIntegrationJournal,
) -> Result<()> {
    for scenario in &report.integrations {
        for service in &scenario.services {
            for observation in &service.observations {
                if let Some(readiness) = &observation.readiness {
                    let probe = journal
                        .readiness_probes
                        .iter()
                        .find(|p| p.observation_ids.contains(&observation.binding.id))
                        .ok_or_else(|| {
                            anyhow::anyhow!("public readiness lacks original private probe")
                        })?;
                    ensure!(
                        probe.challenge.ownership_id == scenario.ownership_id
                            && probe.challenge.slot == service.slot
                            && probe.challenge.lease == observation.binding.lease
                            && probe.challenge.operation == readiness.operation
                            && probe.challenge.digest()? == readiness.challenge_digest
                            && probe.expectation_digest == readiness.expectation_digest
                            && probe.expectation.wire() == readiness.expectation,
                        "public readiness private probe mismatch"
                    );
                    if readiness.state == theseus_native_protocol::v2::ReadinessState::Ready {
                        ensure!(
                            matches!(&probe.completion,Disposition::Complete{receipt} if Some(receipt)==readiness.response_digest.as_ref()),
                            "ready response lacks exact completed private receipt"
                        );
                    }
                }
            }
        }
    }
    Ok(())
}
