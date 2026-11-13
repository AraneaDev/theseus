//! Service inspection, binding publication and precondition observation.
use super::{IntegrationDriver, artifact_str, role_str, runtime_precondition_outcome};
use crate::{
    engine::domain::{StoredJournal, StoredRun},
    model::CheckOutcome,
    native::{integration::model::*, model::Disposition},
};
use anyhow::{Result, bail, ensure};
use theseus_native_client::Action;
use theseus_native_protocol::v2;
impl IntegrationDriver<'_, '_, '_> {
    pub(super) fn inspect(
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
        let (observed, observation) = match reply {
            v2::Reply::ServiceObservation { lease, observation } => (lease, observation),
            // The fixed refusal class is a closed protocol enum, never raw
            // native text; naming it separates manager-inspection failure
            // (OwnershipPending) from Busy/Unsupported/lease refusals.
            v2::Reply::Refused {
                code,
                inspect_failure,
            } => match inspect_failure {
                Some(failure) => {
                    bail!("service inspection refused: {code:?} at {}", failure.name())
                }
                None => bail!("service inspection refused: {code:?}"),
            },
            _ => bail!("service inspection unavailable"),
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
            source_profile: super::super::PROFILE.into(),
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
                            expected.role == role_str(role)
                                && expected.artifact == artifact_str(artifact)
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
    pub(super) fn publish_binding(
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
            let role = role_str(key.role);
            let artifact = artifact_str(key.artifact);
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
    pub(super) fn observe_runtime_precondition(
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
    pub(super) fn inspect_configuration_only(
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
                        expected.role == role_str(key.role)
                            && expected.artifact == artifact_str(key.artifact)
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
