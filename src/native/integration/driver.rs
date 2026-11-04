//! Service hooks around the one ordinary lifecycle sequence.
use super::{ValidatedIntegrationRecipe, model::*};
use crate::{
    engine::{
        common::{self, LifecycleBackend, sealed},
        domain::{StoredJournal, StoredRun},
    },
    model::{CheckOutcome, Checkpoint, ScenarioKind, StepName},
    native::{adapter::NativeDriver, model::Disposition},
};
use anyhow::{Result, bail, ensure};

use std::time::{Duration, Instant};
use theseus_native_client::{Action, Event, Poll};
use theseus_native_protocol::v2;

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
    fn inspect(
        &mut self,
        phase: IntegrationPhase,
        definition: Option<DefinitionKey>,
    ) -> Result<()> {
        let slot = self.service()?.resource.clone();
        let lease = self.core.lease()?;
        let started_ms = self.started.elapsed().as_millis() as u64;
        let request = self.core.session.submit(Action::InspectService {
            lease,
            slot: v2::ResourceSlot::new(slot.as_str())?,
        })?;
        let reply = self.core.wait_reply(request)?;
        let v2::Reply::ServiceObservation {
            lease: observed,
            observation,
        } = reply
        else {
            bail!("service inspection refused or unavailable")
        };
        ensure!(
            observed == lease && observation.slot.as_str() == slot.as_str(),
            "service observation original binding mismatch"
        );
        self.sequence += 1;
        let observation_action = if phase == IntegrationPhase::Uninstalled {
            self.sut_operation()
        } else {
            self.pending_control_observation
                .take()
                .or_else(|| self.activation_action.as_ref().map(|a| a.operation.clone()))
                .or_else(|| self.sut_operation())
        };
        let owner = self.core.resource.resource();
        let binding = ObservationBinding {
            id: format!("service-{}-{}", owner.scenario_id, self.sequence),
            run_id: match self.core.projector.report() {
                StoredRun::NativeIntegration(r) => r.run_id.clone(),
                _ => unreachable!(),
            },
            scenario_id: owner.scenario_id.clone(),
            ownership_id: owner.ownership_id.clone(),
            lease: lease.to_hex(),
            slot: slot.clone(),
            request_id: format!("inspect-{}", self.sequence),
            action: observation_action,
            binding_digest: observation.binding_digest.clone(),
            source_profile: super::PROFILE.into(),
            source_version: self
                .core
                .session
                .integration_description()
                .ok_or_else(|| anyhow::anyhow!("integration description missing"))?
                .manifest
                .service_authority
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("service authority missing"))?
                .api_profile()
                .into(),
            phase,
            definition,
            sequence: self.sequence,
            started_ms,
            finished_ms: self.started.elapsed().as_millis() as u64,
            complete: observation.complete,
        };
        let state = if !observation.complete {
            ServiceState::Unknown {
                reason: "incomplete manager observation".into(),
            }
        } else if !observation.registration_present {
            ServiceState::Absent
        } else {
            ServiceState::Present {
                registered: true,
                configured: observation.configured,
                running: observation.running,
                definition_digest: observation
                    .definition_digest
                    .clone()
                    .unwrap_or_else(|| "00".repeat(32)),
                origin: observation
                    .origin
                    .clone()
                    .unwrap_or_else(|| "manager origin unavailable".into()),
            }
        };
        let bound = BoundServiceObservation {
            platform: observation.platform.clone(),
            readiness: None,
            binding: binding.clone(),
            state: IntegrationProvenance::ManagerReported {
                value: state,
                manager: observation.manager_generation.clone(),
            },
        };
        let requirements = self
            .service()?
            .checks
            .iter()
            .filter(|c| {
                c.scenarios.contains(&self.scenario)
                    && c.phase == phase
                    && c.predicate != ServicePredicate::ApplicationReady
            })
            .cloned()
            .collect::<Vec<_>>();
        let sut_operation = self.sut_operation();
        let existing_checks = self
            .evidence_mut()?
            .checks
            .iter()
            .map(|c| c.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let observed_manifest = self
            .core
            .session
            .integration_description()
            .map(|d| d.manifest.clone());
        let activation = self.activation_cause;
        let evidence = self.evidence_mut()?;
        if evidence.services.is_empty() {
            evidence.services.push(ServiceEvidence {
                slot: slot.clone(),
                observations: vec![],
                actions: vec![],
            });
        }
        evidence.services[0].observations.push(bound);
        for required in requirements {
            if existing_checks.contains(&required.id) {
                continue;
            }
            let definition_matches = match (required.role, required.artifact) {
                (Some(role), Some(artifact)) => {
                    observed_manifest.as_ref().is_some_and(|manifest| {
                        manifest.definitions.iter().any(|expected| {
                            expected.role
                                == match role {
                                    DefinitionRole::Base => "base",
                                    DefinitionRole::Configured => "configured",
                                    DefinitionRole::Target => "target",
                                }
                                && expected.artifact
                                    == match artifact {
                                        ArtifactRole::Base => "base",
                                        ArtifactRole::Target => "target",
                                    }
                                && Some(&expected.definition_sha256)
                                    == observation.definition_digest.as_ref()
                        })
                    })
                }
                (None, None) => true,
                _ => false,
            };
            let passed = observation.complete
                && definition_matches
                && match required.predicate {
                    ServicePredicate::Registration => observation.registration_present,
                    ServicePredicate::Configuration => {
                        observation.registration_present && observation.configured
                    }
                    ServicePredicate::Running => observation.running,
                    ServicePredicate::Stopped => !observation.running,
                    ServicePredicate::RegistrationAbsent => !observation.registration_present,
                    ServicePredicate::ApplicationReady => false,
                };
            evidence.checks.push(IntegrationCheck {
                id: required.id,
                scenario_id: binding.scenario_id.clone(),
                phase,
                slot: slot.clone(),
                role: required.role,
                artifact: required.artifact,
                predicate: IntegrationPredicate::Service(required.predicate),
                origin: if matches!(
                    required.predicate,
                    ServicePredicate::Running | ServicePredicate::ApplicationReady
                ) && activation == ActivationCause::RehearsalStart
                {
                    CheckOrigin::RehearsalControl
                } else {
                    CheckOrigin::SutPostcondition
                },
                sut_operation: sut_operation.clone(),
                observation_ids: vec![binding.id.clone()],
                outcome: if passed {
                    CheckOutcome::Passed
                } else if observation.complete {
                    CheckOutcome::Failed
                } else {
                    CheckOutcome::Incomplete
                },
                detail: if passed {
                    "declared service predicate observed"
                } else {
                    "declared service predicate failed or unavailable"
                }
                .into(),
            });
        }
        self.last_observation = Some(*observation);
        self.core.projector.save_report()
    }
    fn publish_binding(
        &mut self,
        basis: BindingBasis,
        definition: Option<DefinitionKey>,
    ) -> Result<()> {
        let basis = if basis == BindingBasis::ExclusiveOwnedRegistration
            && self
                .core
                .session
                .integration_description()
                .is_some_and(|d| {
                    d.manifest
                        .service_authority
                        .as_ref()
                        .is_some_and(|a| a.platform() == v2::ManagerPlatform::Scm)
                }) {
            BindingBasis::LiveScmObject
        } else {
            basis
        };
        let observed = self
            .last_observation
            .clone()
            .ok_or_else(|| anyhow::anyhow!("service observation missing"))?;
        if matches!(
            basis,
            BindingBasis::ExclusiveOwnedRegistration | BindingBasis::LiveScmObject
        ) {
            let key = definition.as_ref().ok_or_else(|| {
                anyhow::anyhow!("registration binding requires exact definition key")
            })?;
            let role = match key.role {
                DefinitionRole::Base => "base",
                DefinitionRole::Configured => "configured",
                DefinitionRole::Target => "target",
            };
            let artifact = match key.artifact {
                ArtifactRole::Base => "base",
                ArtifactRole::Target => "target",
            };
            ensure!(
                observed.complete
                    && observed.registration_present
                    && self
                        .core
                        .session
                        .integration_description()
                        .is_some_and(|d| d.manifest.definitions.iter().any(|expected| expected
                            .role
                            == role
                            && expected.artifact == artifact
                            && Some(&expected.definition_sha256)
                                == observed.definition_digest.as_ref())),
                "exact declared registration definition is unproved before rehearsal"
            );
        }
        if basis == BindingBasis::LiveScmObject {
            ensure!(
                matches!(
                    &observed.platform,
                    Some(v2::ServicePlatformFacts::Scm {
                        original_handle_retained: true,
                        ..
                    })
                ),
                "live SCM object binding requires original retained service handle"
            );
        }
        let slot = self.service()?.resource.clone();
        let receipt_digest = self
            .core
            .session
            .integration_description()
            .ok_or_else(|| anyhow::anyhow!("integration context description missing"))?
            .namespace_receipt_digest
            .to_hex();
        let observation_ids = self.evidence_mut()?.services[0]
            .observations
            .last()
            .map(|o| vec![o.binding.id.clone()])
            .unwrap_or_default();
        let owner = self.core.resource.resource();
        let snapshot = ResourceBindingSnapshot {
            digest: observed.binding_digest.clone(),
            basis,
            evidence_ids: observation_ids,
            definition,
            artifact_commitment: self.recipe.core.artifact_commitment()?.to_hex(),
            namespace_or_session_receipt_digest: receipt_digest,
            original_action: if basis == BindingBasis::VacantRegistration {
                self.sut_operation()
            } else {
                self.activation_action
                    .as_ref()
                    .map(|a| a.operation.clone())
                    .or_else(|| self.sut_operation())
            },
        };
        let mut record = match self.core.projector.journal() {
            StoredJournal::NativeIntegration(j) => j
                .integration_ownership
                .iter()
                .find(|r| r.key.ownership_id == owner.ownership_id && r.key.slot == slot)
                .cloned(),
            _ => None,
        }
        .unwrap_or(IntegrationResourceRecord {
            key: IntegrationResourceKey {
                scenario_id: owner.scenario_id.clone(),
                ownership_id: owner.ownership_id.clone(),
                slot,
            },
            kind: IntegrationResourceKind::Service,
            manifest_digest: self.recipe.full_manifest_digest()?,
            initial_binding: snapshot.clone(),
            binding_history: vec![],
            current_binding_digest: snapshot.digest.clone(),
            revocation: None,
            disposition: Disposition::Unknown {
                reason: "original service registration postcondition pending".into(),
            },
        });
        // Failed removal never creates a fresh vacant-registration authority.
        // Keep the original binding history; the failed observation remains in
        // the report and the resource retains its independent failed state.
        if basis == BindingBasis::VacantRegistration
            && !record.binding_history.is_empty()
            && (!observed.complete || observed.registration_present || observed.running)
        {
            record.disposition = if observed.complete {
                Disposition::Failed {
                    reason: "SUT left original registration or runtime present".into(),
                }
            } else {
                Disposition::Unknown{reason:"SUT removal observation incomplete; original registration authority retained".into()}
            };
            return self.core.projector.publish_integration_resource(record);
        }
        record.binding_history.push(snapshot.clone());
        record.current_binding_digest = snapshot.digest;
        if matches!(
            basis,
            BindingBasis::ExclusiveOwnedRegistration | BindingBasis::LiveScmObject
        ) {
            record.disposition = Disposition::Complete {
                receipt: if basis == BindingBasis::LiveScmObject {
                    "original SUT registration observed through retained SCM handle"
                } else {
                    "original SUT registration observed in exclusive namespace"
                }
                .into(),
            };
        }
        if basis == BindingBasis::VacantRegistration && record.binding_history.len() > 1 {
            record.disposition = if !observed.registration_present && !observed.running {
                Disposition::Complete {
                    receipt: "SUT registration and runtime absent before provider disposal".into(),
                }
            } else {
                Disposition::Failed {
                    reason: "SUT left original registration or runtime present".into(),
                }
            };
        }
        self.core.projector.publish_integration_resource(record)
    }
    fn control(&mut self, action: ServiceAction) -> Result<()> {
        self.core.reserve_operation()?;
        let lease = self.core.lease()?;
        let slot = self.service()?.resource.clone();
        let prior = self
            .last_observation
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("service control requires complete observed binding"))?
            .binding_digest
            .clone();
        let mut nonce = [0u8; 16];
        theseus_host::random_bytes(&mut nonce)?;
        let operation = v2::OperationId::from_bytes(nonce);
        let wire_action = match action {
            ServiceAction::Start => v2::ServiceAction::Start,
            ServiceAction::Stop => v2::ServiceAction::Stop,
        };
        let request = v2::Request::ServiceAction {
            lease,
            operation,
            slot: v2::ResourceSlot::new(slot.as_str())?,
            prior_binding_digest: v2::Digest::parse(&prior)?,
            action: wire_action,
        };
        let digest = v2::request_digest(&request)?.to_hex();
        let record = IntegrationActionRecord {
            ownership_id: self.core.resource.resource().ownership_id.clone(),
            lease: lease.to_hex(),
            slot: slot.clone(),
            operation: operation.to_hex(),
            request: IntegrationRequest::Service {
                lease: lease.to_hex(),
                operation: operation.to_hex(),
                slot,
                prior_binding_digest: prior.clone(),
                control: action,
            },
            request_digest: digest.clone(),
            prior_binding_digest: prior.clone(),
            broker_identity: None,
            completion: Disposition::Unknown {
                reason: "published; native action unproved".into(),
            },
            terminal: None,
        };
        let saved = self
            .core
            .projector
            .publish_integration_action(&self.core.resource, record)?
            .into_action();
        self.core.session.submit(Action::ServiceAction {
            lease,
            operation,
            slot: v2::ResourceSlot::new(saved.slot.as_str())?,
            prior_binding_digest: v2::Digest::parse(&prior)?,
            action: wire_action,
        })?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let terminal = loop {
            ensure!(
                Instant::now() < deadline,
                "original service action remains unproved"
            );
            match self.core.session.poll()? {
                Poll::Event(Event::Reply {
                    reply:
                        v2::Reply::Operation {
                            lease: l,
                            operation: o,
                            state:
                                v2::OperationView::Finished {
                                    outcome: v2::Outcome::ServiceActionCompleted { outcome },
                                },
                        },
                    ..
                }) if l == lease && o == operation => break outcome,
                Poll::Event(Event::Reply {
                    reply: v2::Reply::Refused { code },
                    ..
                }) => bail!("service control refused: {code:?}"),
                Poll::Event(Event::ConnectionLost | Event::PeerClosed(_))
                | Poll::TransportSettled => bail!("original service action transport lost"),
                _ => {}
            }
            // Query the same accepted operation; never resubmit the native effect.
            std::thread::sleep(Duration::from_millis(25));
            if let Ok(id) = self.core.session.submit(Action::Query { lease, operation })
                && let v2::Reply::Operation {
                    state:
                        v2::OperationView::Finished {
                            outcome: v2::Outcome::ServiceActionCompleted { outcome },
                        },
                    ..
                } = self.core.wait_reply(id)?
            {
                break outcome;
            }
        };
        let (completion, outcome) = match terminal {
            v2::ServiceOutcome::Complete { receipt } => (
                Disposition::Complete {
                    receipt: receipt.clone(),
                },
                IntegrationOutcome::Complete { receipt },
            ),
            v2::ServiceOutcome::Failed { reason } => (
                Disposition::Failed {
                    reason: reason.clone(),
                },
                IntegrationOutcome::Failed { reason },
            ),
            v2::ServiceOutcome::Unknown { reason } => (
                Disposition::Unknown {
                    reason: reason.clone(),
                },
                IntegrationOutcome::Unknown { reason },
            ),
        };
        self.core.projector.record_integration_completion(
            &lease.to_hex(),
            &operation.to_hex(),
            completion.clone(),
            outcome,
        )?;
        self.evidence_mut()?.services[0]
            .actions
            .push(ActionReference {
                operation: operation.to_hex(),
                request_digest: digest.clone(),
            });
        self.evidence_mut()?.dispositions.manager_action = completion.clone();
        self.core.projector.save_report()?;
        ensure!(
            matches!(completion, Disposition::Complete { .. }),
            "original service action failed or remains unknown"
        );
        self.pending_control_observation = Some(operation.to_hex());
        if action == ServiceAction::Start {
            self.activation_cause = ActivationCause::RehearsalStart;
            self.activation_action = Some(ActionReference {
                operation: operation.to_hex(),
                request_digest: digest,
            });
        }
        Ok(())
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
// One step/checkpoint compositor around the original core; no scenario or owner loop.
trait OrdinaryIntegrationHooks {
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
fn composed_step(h: &mut impl OrdinaryIntegrationHooks, step: StepName, name: &str) -> Result<()> {
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
fn composed_checkpoint(
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
fn composed_before_reinstall(h: &mut impl OrdinaryIntegrationHooks) -> Result<()> {
    if h.forward_hooks() {
        h.core_before_reinstall()?;
    }
    Ok(())
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
    fn inspect_deferred_cleanup_authority(&mut self) -> Result<()> {
        let owner = self.core.resource.resource();
        owner
            .catalog_authority
            .authorize(crate::native::model::CatalogProgram::Uninstall)?;
        let (revoked, owned_registration) = match self.core.projector.journal() {
            StoredJournal::NativeIntegration(journal) => {
                let records = journal
                    .integration_ownership
                    .iter()
                    .filter(|r| {
                        r.key.ownership_id == owner.ownership_id
                            && r.kind == IntegrationResourceKind::Service
                    })
                    .collect::<Vec<_>>();
                (
                    records.iter().any(|r| r.revocation.is_some()),
                    records.iter().any(|r| {
                        r.binding_history.iter().any(|b| {
                            matches!(
                                b.basis,
                                BindingBasis::ExclusiveOwnedRegistration
                                    | BindingBasis::LiveScmObject
                            )
                        })
                    }),
                )
            }
            _ => (true, false),
        };
        ensure!(
            !revoked,
            "service authority revoked; cleanup is provider-only"
        );
        let definition = self
            .evidence_mut()?
            .services
            .first()
            .and_then(|s| s.observations.last())
            .and_then(|o| o.binding.definition.clone());
        // No Running requirement and no rehearsal Stop: this is solely a fresh
        // read against the original manager owner before the retained SUT Run.
        self.inspect(IntegrationPhase::BeforeUninstall, definition)?;
        ensure!(
            self.last_observation
                .as_ref()
                .is_some_and(|o| o.complete
                    && (!o.registration_present || (owned_registration && o.configured))),
            "original service cleanup owner unproved; cleanup is provider-only"
        );
        Ok(())
    }
    fn observe_runtime_precondition(
        &mut self,
        name: &str,
        precondition: RuntimePrecondition,
        phase: IntegrationPhase,
        artifact: ArtifactRole,
    ) -> Result<()> {
        if precondition == RuntimePrecondition::HarnessStopped {
            self.control(ServiceAction::Stop)?;
        }
        self.inspect(
            phase,
            Some(DefinitionKey {
                role: DefinitionRole::Configured,
                artifact,
            }),
        )?;
        let observation = self
            .last_observation
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("runtime precondition observation missing"))?;
        let outcome =
            runtime_precondition_outcome(observation.complete, observation.running, precondition);
        let binding = self.evidence_mut()?.services[0]
            .observations
            .last()
            .unwrap()
            .binding
            .clone();
        self.evidence_mut()?.checks.push(IntegrationCheck {
            id: format!("{name}-runtime-precondition"),
            scenario_id: binding.scenario_id,
            phase,
            slot: binding.slot,
            role: Some(DefinitionRole::Configured),
            artifact: Some(artifact),
            predicate: IntegrationPredicate::Service(
                if precondition == RuntimePrecondition::Running {
                    ServicePredicate::Running
                } else {
                    ServicePredicate::Stopped
                },
            ),
            origin: CheckOrigin::RehearsalControl,
            sut_operation: None,
            observation_ids: vec![binding.id],
            outcome,
            detail: if precondition == RuntimePrecondition::Running {
                format!("fresh declared Running {name} precondition; no harness Stop")
            } else {
                format!("explicit HarnessStopped {name} precondition; no installer-stop proof")
            },
        });
        self.core.projector.save_report()?;
        ensure!(
            outcome == CheckOutcome::Passed,
            "declared {name} runtime precondition failed or remains unknown"
        );
        Ok(())
    }
    fn inspect_configuration_only(
        &mut self,
        phase: IntegrationPhase,
        definition: Option<DefinitionKey>,
    ) -> Result<()> {
        self.inspect(
            IntegrationPhase::ConfiguredBeforeActivation,
            definition.clone(),
        )?;
        let observation = self
            .last_observation
            .clone()
            .ok_or_else(|| anyhow::anyhow!("pre-control configuration observation missing"))?;
        let mut bound = self.evidence_mut()?.services[0]
            .observations
            .last()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("pre-control evidence missing"))?;
        bound.binding.id.push_str("-configuration");
        bound.binding.phase = phase;
        let required = self
            .service()?
            .checks
            .iter()
            .filter(|c| {
                c.scenarios.contains(&self.scenario)
                    && c.phase == phase
                    && matches!(
                        c.predicate,
                        ServicePredicate::Configuration | ServicePredicate::Registration
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        let matched = definition.as_ref().is_some_and(|key| {
            self.core
                .session
                .integration_description()
                .is_some_and(|d| {
                    d.manifest.definitions.iter().any(|expected| {
                        expected.role
                            == match key.role {
                                DefinitionRole::Base => "base",
                                DefinitionRole::Configured => "configured",
                                DefinitionRole::Target => "target",
                            }
                            && expected.artifact
                                == match key.artifact {
                                    ArtifactRole::Base => "base",
                                    ArtifactRole::Target => "target",
                                }
                            && Some(&expected.definition_sha256)
                                == observation.definition_digest.as_ref()
                    })
                })
        });
        let sut_operation = self.sut_operation();
        let evidence = self.evidence_mut()?;
        for requirement in required {
            evidence.checks.push(IntegrationCheck {
                id: requirement.id,
                scenario_id: bound.binding.scenario_id.clone(),
                phase,
                slot: bound.binding.slot.clone(),
                role: requirement.role,
                artifact: requirement.artifact,
                predicate: IntegrationPredicate::Service(requirement.predicate),
                origin: CheckOrigin::SutPostcondition,
                sut_operation: sut_operation.clone(),
                observation_ids: vec![bound.binding.id.clone()],
                outcome: if !observation.complete {
                    CheckOutcome::Incomplete
                } else if matched && observation.configured {
                    CheckOutcome::Passed
                } else {
                    CheckOutcome::Failed
                },
                detail: "fresh exact SUT configuration observed before rehearsal control".into(),
            });
        }
        evidence.services[0].observations.push(bound);
        self.core.projector.save_report()?;
        Ok(())
    }
}

impl IntegrationDriver<'_, '_, '_> {
    fn probe_readiness(
        &mut self,
        phase: IntegrationPhase,
        definition: DefinitionKey,
    ) -> Result<()> {
        let service = self.service()?.clone();
        let required = service
            .checks
            .iter()
            .filter(|c| {
                c.scenarios.contains(&self.scenario)
                    && c.phase == phase
                    && c.predicate == ServicePredicate::ApplicationReady
            })
            .cloned()
            .collect::<Vec<_>>();
        if required.is_empty() {
            return Ok(());
        }
        ensure!(
            service.readiness.kind == ReadyKind::LocalChallenge,
            "application readiness requires explicit local challenge"
        );
        let role = match definition.role {
            DefinitionRole::Base => "base",
            DefinitionRole::Configured => "configured",
            DefinitionRole::Target => "target",
        };
        let artifact = match definition.artifact {
            ArtifactRole::Base => "base",
            ArtifactRole::Target => "target",
        };
        let expected = self
            .core
            .session
            .integration_description()
            .and_then(|d| d.manifest.readiness.as_ref())
            .and_then(|r| {
                r.expectations
                    .iter()
                    .find(|e| e.role == role && e.artifact == artifact)
            })
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("authenticated readiness expectation missing"))?;
        let pinned = if definition.artifact == ArtifactRole::Base {
            &self.recipe.core.recipe.base
        } else {
            &self.recipe.core.recipe.target
        };
        ensure!(
            expected.artifact_length == pinned.length && expected.artifact_sha256 == pinned.sha256,
            "readiness artifact differs from admitted core artifact"
        );
        let action = self.activation_action.clone().ok_or_else(|| {
            anyhow::anyhow!("readiness requires current original SUT or rehearsal activation")
        })?;
        let lease = self.core.lease()?;
        let prior = self
            .last_observation
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("readiness service observation missing"))?
            .binding_digest
            .clone();
        let mut nonce = [0u8; 16];
        theseus_host::random_bytes(&mut nonce)?;
        let nonce = v2::BindingId::from_bytes(nonce);
        let expectation = ReadinessExpectation {
            role: definition.role,
            artifact: definition.artifact,
            artifact_length: expected.artifact_length,
            configuration_digest: expected.configuration_digest,
            artifact_digest: expected.artifact_sha256,
            version: expected.version_literal,
            data_digest: expected.data_sha256,
            initialized: true,
        };
        let expectation_digest = expectation.digest()?;
        let challenge = ReadinessChallenge {
            ownership_id: self.core.resource.resource().ownership_id.clone(),
            lease: lease.to_hex(),
            slot: service.resource.clone(),
            operation: action.operation.clone(),
            nonce: nonce.to_hex(),
            deadline_ms: service.readiness.deadline_ms,
            resource_binding_digest: prior.clone(),
            action_digest: action.request_digest.clone(),
            expectation_digest: expectation_digest.clone(),
        };
        self.core
            .projector
            .publish_readiness_probe(ReadinessProbeRecord {
                challenge: challenge.clone(),
                expectation,
                expectation_digest: expectation_digest.clone(),
                observation_ids: vec![],
                completion: Disposition::Unknown {
                    reason: "published; authenticated response unproved".into(),
                },
            })?;
        let started_ms = self.started.elapsed().as_millis() as u64;
        let request = self.core.session.submit(Action::ProbeReadiness {
            deadline_ms: service.readiness.deadline_ms,
            lease,
            slot: v2::ResourceSlot::new(service.resource.as_str())?,
            operation: v2::OperationId::parse(&action.operation)?,
            action_digest: v2::Digest::parse(&action.request_digest)?,
            prior_binding_digest: v2::Digest::parse(&prior)?,
            nonce,
            definition_role: role.into(),
            artifact_role: artifact.into(),
        })?;
        let v2::Reply::ReadinessObservation {
            lease: observed,
            observation,
        } = self.core.wait_reply(request)?
        else {
            bail!("readiness probe refused or incomplete")
        };
        ensure!(
            observed == lease
                && observation.expectation_digest == expectation_digest
                && observation.challenge_digest == challenge.digest()?
                && observation.resource_binding_digest == prior
                && observation.action_digest == action.request_digest
                && observation.operation == action.operation,
            "readiness response original correlation differs"
        );
        self.sequence += 1;
        let sequence = self.sequence;
        let prior_observation = self.evidence_mut()?.services[0]
            .observations
            .last()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("readiness resource observation absent"))?;
        let mut binding = prior_observation.binding;
        binding.id = format!("readiness-{}-{}", binding.scenario_id, sequence);
        binding.request_id = format!("probe-{sequence}");
        binding.phase = phase;
        binding.definition = Some(definition);
        binding.action = Some(action.operation.clone());
        binding.sequence = sequence;
        binding.started_ms = started_ms;
        binding.finished_ms = self.started.elapsed().as_millis() as u64;
        binding.complete = observation.state != v2::ReadinessState::Incomplete;
        let outcome = match observation.state {
            v2::ReadinessState::Ready
                if observation.authenticated_instance && observation.initialized == Some(true) =>
            {
                CheckOutcome::Passed
            }
            v2::ReadinessState::Failed => CheckOutcome::Failed,
            _ => CheckOutcome::Incomplete,
        };
        let completion = match outcome {
            CheckOutcome::Passed => Disposition::Complete {
                receipt: observation
                    .response_digest
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("ready response digest absent"))?,
            },
            CheckOutcome::Failed => Disposition::Failed {
                reason: observation
                    .failure
                    .clone()
                    .unwrap_or_else(|| "application readiness assertion failed".into()),
            },
            _ => Disposition::Unknown {
                reason: "application readiness response incomplete".into(),
            },
        };
        self.core.projector.record_readiness_probe_completion(
            &challenge.ownership_id,
            &challenge.operation,
            &challenge.nonce,
            &expectation_digest,
            vec![binding.id.clone()],
            completion,
        )?;
        let sut_operation = self.sut_operation();
        let activation = self.activation_cause;
        let evidence = self.evidence_mut()?;
        for requirement in required {
            evidence.checks.push(IntegrationCheck{id:requirement.id,scenario_id:binding.scenario_id.clone(),phase,slot:service.resource.clone(),role:requirement.role,artifact:requirement.artifact,predicate:IntegrationPredicate::Service(ServicePredicate::ApplicationReady),origin:if activation==ActivationCause::Sut { CheckOrigin::SutPostcondition } else { CheckOrigin::RehearsalControl },sut_operation:sut_operation.clone(),observation_ids:vec![binding.id.clone()],outcome,detail:if outcome==CheckOutcome::Passed{"authenticated application response matched artifact, version, data and configuration"}else{"authenticated application readiness failed or remains incomplete"}.into()});
        }
        evidence.services[0]
            .observations
            .push(BoundServiceObservation {
                platform: prior_observation.platform,
                binding,
                state: prior_observation.state,
                readiness: Some(*observation),
            });
        self.core.projector.save_report()
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
