//! Authenticated application readiness probing.
use super::{IntegrationDriver, artifact_str, role_str};
use crate::{
    model::CheckOutcome,
    native::{integration::model::*, model::Disposition},
};
use anyhow::{Result, bail, ensure};
use theseus_native_client::Action;
use theseus_native_protocol::v2;
impl IntegrationDriver<'_, '_, '_> {
    pub(super) fn probe_readiness(
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
        let role = role_str(definition.role);
        let artifact = artifact_str(definition.artifact);
        let expected = self.readiness_expectation(&definition, role, artifact)?;
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
        let challenge = self.readiness_challenge(
            &service,
            &action,
            lease,
            &nonce,
            &prior,
            &expectation_digest,
        );
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
        validate_readiness_reply(
            observed,
            lease,
            &observation,
            &expectation_digest,
            &challenge,
            &prior,
            &action,
        )?;
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
        let bound = BoundServiceObservation {
            platform: prior_observation.platform,
            binding,
            state: prior_observation.state,
            readiness: Some(*observation),
        };
        self.push_readiness_checks(required, phase, &service, outcome, bound)
    }
    /// Authenticated manifest expectation for the declared definition, which
    /// must match the admitted core artifact.
    fn readiness_expectation(
        &self,
        definition: &DefinitionKey,
        role: &str,
        artifact: &str,
    ) -> Result<v2::ReadinessDefinitionExpectation> {
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
        Ok(expected)
    }
    fn readiness_challenge(
        &self,
        service: &ServiceRequirement,
        action: &ActionReference,
        lease: v2::LeaseId,
        nonce: &v2::BindingId,
        prior: &str,
        expectation_digest: &str,
    ) -> ReadinessChallenge {
        ReadinessChallenge {
            ownership_id: self.core.resource.resource().ownership_id.clone(),
            lease: lease.to_hex(),
            slot: service.resource.clone(),
            operation: action.operation.clone(),
            nonce: nonce.to_hex(),
            deadline_ms: service.readiness.deadline_ms,
            resource_binding_digest: prior.to_owned(),
            action_digest: action.request_digest.clone(),
            expectation_digest: expectation_digest.to_owned(),
        }
    }
    // The activation cause and SUT operation are read before the evidence
    // borrow is taken, exactly as in the original single-function flow.
    fn push_readiness_checks(
        &mut self,
        required: Vec<ServiceCheckRequirement>,
        phase: IntegrationPhase,
        service: &ServiceRequirement,
        outcome: CheckOutcome,
        bound: BoundServiceObservation,
    ) -> Result<()> {
        let sut_operation = self.sut_operation();
        let activation = self.activation_cause;
        let evidence = self.evidence_mut()?;
        for requirement in required {
            evidence.checks.push(IntegrationCheck{id:requirement.id,scenario_id:bound.binding.scenario_id.clone(),phase,slot:service.resource.clone(),role:requirement.role,artifact:requirement.artifact,predicate:IntegrationPredicate::Service(ServicePredicate::ApplicationReady),origin:if activation==ActivationCause::Sut { CheckOrigin::SutPostcondition } else { CheckOrigin::RehearsalControl },sut_operation:sut_operation.clone(),observation_ids:vec![bound.binding.id.clone()],outcome,detail:if outcome==CheckOutcome::Passed{"authenticated application response matched artifact, version, data and configuration"}else{"authenticated application readiness failed or remains incomplete"}.into()});
        }
        evidence.services[0].observations.push(bound);
        self.core.projector.save_report()
    }
}
fn validate_readiness_reply(
    observed: v2::LeaseId,
    lease: v2::LeaseId,
    observation: &v2::ReadinessObservation,
    expectation_digest: &str,
    challenge: &ReadinessChallenge,
    prior: &str,
    action: &ActionReference,
) -> Result<()> {
    ensure!(
        observed == lease
            && observation.expectation_digest == expectation_digest
            && observation.challenge_digest == challenge.digest()?
            && observation.resource_binding_digest == prior
            && observation.action_digest == action.request_digest
            && observation.operation == action.operation,
        "readiness response original correlation differs"
    );
    Ok(())
}
