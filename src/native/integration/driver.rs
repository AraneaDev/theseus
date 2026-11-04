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
    started: Instant,
}
impl<'a, 'b, 's> IntegrationDriver<'a, 'b, 's> {
    pub fn new(core: &'a mut NativeDriver<'b, 's>, recipe: &'a ValidatedIntegrationRecipe) -> Self {
        Self {
            core,
            recipe,
            scenario: ScenarioKind::Clean,
            sequence: 0,
            last_observation: None,
            first_install: true,
            started: Instant::now(),
        }
    }
    pub fn run(&mut self, scenario: ScenarioKind) -> Result<()> {
        ensure!(
            self.recipe.recipe.integrations.desktop.is_none(),
            "desktop integration requires explicit session capability"
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
        common::run_common(self, scenario)
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
            action: self.sut_operation(),
            binding_digest: observation.binding_digest.clone(),
            source_profile: super::PROFILE.into(),
            source_version: self
                .core
                .session
                .integration_description()
                .ok_or_else(|| anyhow::anyhow!("integration description missing"))?
                .manifest
                .service_authority
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
                origin: CheckOrigin::SutPostcondition,
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
                    d.manifest.service_authority.platform() == v2::ManagerPlatform::Scm
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
            original_action: self.sut_operation(),
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
                request_digest: digest,
            });
        self.evidence_mut()?.dispositions.manager_action = completion.clone();
        self.core.projector.save_report()?;
        ensure!(
            matches!(completion, Disposition::Complete { .. }),
            "original service action failed or remains unknown"
        );
        Ok(())
    }
}
impl sealed::Sealed for IntegrationDriver<'_, '_, '_> {}
impl LifecycleBackend for IntegrationDriver<'_, '_, '_> {
    fn step(&mut self, step: StepName, name: &str) -> Result<()> {
        if step == StepName::Uninstall {
            let precondition = self.service()?.controls.uninstall_precondition;
            if precondition == RuntimePrecondition::HarnessStopped {
                self.control(ServiceAction::Stop)?;
            }
            self.inspect(
                IntegrationPhase::BeforeUninstall,
                Some(DefinitionKey {
                    role: DefinitionRole::Configured,
                    artifact: ArtifactRole::Target,
                }),
            )?;
            let observation = self
                .last_observation
                .clone()
                .ok_or_else(|| anyhow::anyhow!("uninstall precondition observation missing"))?;
            let binding = self.evidence_mut()?.services[0]
                .observations
                .last()
                .unwrap()
                .binding
                .clone();
            let matched = observation.complete
                && if precondition == RuntimePrecondition::Running {
                    observation.running
                } else {
                    !observation.running
                };
            let outcome = if !observation.complete {
                CheckOutcome::Incomplete
            } else if matched {
                CheckOutcome::Passed
            } else {
                CheckOutcome::Failed
            };
            self.evidence_mut()?.checks.push(IntegrationCheck{id:"uninstall-runtime-precondition".into(),scenario_id:binding.scenario_id,phase:IntegrationPhase::BeforeUninstall,slot:binding.slot,role:Some(DefinitionRole::Configured),artifact:Some(ArtifactRole::Target),predicate:IntegrationPredicate::Service(if precondition==RuntimePrecondition::Running{ServicePredicate::Running}else{ServicePredicate::Stopped}),origin:CheckOrigin::RehearsalControl,sut_operation:None,observation_ids:vec![binding.id],outcome,detail:if precondition==RuntimePrecondition::Running{"fresh manager observation of declared Running uninstall precondition"}else{"fresh manager observation of explicit HarnessStopped uninstall precondition; no SUT-stop proof"}.into()});
            self.core.projector.save_report()?;
            ensure!(
                matched,
                "declared uninstall runtime precondition failed or remains unknown"
            );
        }
        self.core.step(step, name)?;
        if step == StepName::Exercise {
            self.inspect(
                IntegrationPhase::ExerciseFinal,
                Some(DefinitionKey {
                    role: DefinitionRole::Configured,
                    artifact: ArtifactRole::Target,
                }),
            )?;
            self.probe_readiness(
                IntegrationPhase::ExerciseFinal,
                DefinitionKey {
                    role: DefinitionRole::Configured,
                    artifact: ArtifactRole::Target,
                },
            )?;
        }
        Ok(())
    }
    fn checkpoint(&mut self, checkpoint: Checkpoint, configured: bool) -> Result<()> {
        self.core.checkpoint(checkpoint, configured)?;
        let phase = match checkpoint {
            Checkpoint::BaseInstalled => IntegrationPhase::BaseInstalled,
            Checkpoint::TargetInstalled if self.first_install => {
                IntegrationPhase::TargetInstalledInitial
            }
            Checkpoint::TargetInstalled => IntegrationPhase::TargetInstalled,
            Checkpoint::Configured => IntegrationPhase::Configured,
            Checkpoint::Reinstalled => IntegrationPhase::Reinstalled,
            Checkpoint::Uninstalled => IntegrationPhase::Uninstalled,
            _ => bail!("unsupported integration checkpoint"),
        };
        let definition = if phase == IntegrationPhase::Uninstalled {
            None
        } else {
            Some(DefinitionKey {
                role: if configured {
                    DefinitionRole::Configured
                } else if checkpoint == Checkpoint::BaseInstalled {
                    DefinitionRole::Base
                } else {
                    DefinitionRole::Target
                },
                artifact: if self.scenario == ScenarioKind::Upgrade
                    && (configured || checkpoint == Checkpoint::BaseInstalled)
                {
                    ArtifactRole::Base
                } else {
                    ArtifactRole::Target
                },
            })
        };
        if configured
            && self.service()?.controls.configured_activation == ActivationCause::RehearsalStart
        {
            self.inspect_configuration_only(phase, definition.clone())?;
            self.publish_binding(BindingBasis::ExclusiveOwnedRegistration, definition.clone())?;
            self.control(ServiceAction::Start)?;
        }
        self.inspect(phase, definition.clone())?;
        if configured {
            self.publish_binding(BindingBasis::ExclusiveOwnedRegistration, definition.clone())?;
        }
        if configured {
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
            self.evidence_mut()?.dispositions.registration = if absent {
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
    fn before_reinstall(&mut self) -> Result<()> {
        self.core.before_reinstall()
    }
}
impl IntegrationDriver<'_, '_, '_> {
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
                    && c.predicate == ServicePredicate::Configuration
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
                predicate: IntegrationPredicate::Service(ServicePredicate::Configuration),
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
        let action = self.evidence_mut()?.services[0]
            .actions
            .last()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("readiness requires original Start action"))?;
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
        let evidence = self.evidence_mut()?;
        for requirement in required {
            evidence.checks.push(IntegrationCheck{id:requirement.id,scenario_id:binding.scenario_id.clone(),phase,slot:service.resource.clone(),role:requirement.role,artifact:requirement.artifact,predicate:IntegrationPredicate::Service(ServicePredicate::ApplicationReady),origin:CheckOrigin::SutPostcondition,sut_operation:sut_operation.clone(),observation_ids:vec![binding.id.clone()],outcome,detail:if outcome==CheckOutcome::Passed{"authenticated application response matched artifact, version, data and configuration"}else{"authenticated application readiness failed or remains incomplete"}.into()});
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
