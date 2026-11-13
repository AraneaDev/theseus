use super::super::*;
use super::common::common_validate;
use crate::model::ScenarioKind;
use crate::native::model::*;
use anyhow::{Result, ensure};
use std::collections::{BTreeMap, BTreeSet};

impl NativeIntegrationJournal {
    pub fn validate(&self) -> Result<()> {
        common_validate(
            (&self.kind, self.schema),
            "theseus_native_journal",
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
        if let Some(fingerprint) = &self.launch_fingerprint {
            digest(fingerprint)?;
        } else {
            ensure!(
                self.ownership.is_empty()
                    && self.operations.is_empty()
                    && self.provider_calls.is_empty()
                    && self.releases.is_empty()
                    && self.integration_ownership.is_empty()
                    && self.integration_actions.is_empty()
                    && self.broker_bindings.is_empty()
                    && self.readiness_probes.is_empty()
                    && self.integration_recoveries.is_empty(),
                "unbound integration journal contains effects"
            );
        }
        ensure!(
            self.ownership.len() <= 3
                && self.provider_calls.len() <= 12
                && self.releases.len() <= 3
                && self.integration_ownership.len() <= 6,
            "integration journal resource count exceeds bound"
        );
        let mut owners = BTreeSet::new();
        for owner in &self.ownership {
            owner.validate()?;
            ensure!(
                owners.insert(&owner.ownership_id),
                "duplicate integration owner"
            );
            match &owner.ownership {
                NativeOwnership::Intent { prepared }
                | NativeOwnership::Acquired { prepared, .. } => ensure!(
                    prepared.manifest_digest == self.manifest_digest,
                    "owner full manifest mismatch"
                ),
                NativeOwnership::Preparing {
                    receipt: Some(r), ..
                } => ensure!(
                    r.manifest_digest == self.manifest_digest,
                    "provider full manifest mismatch"
                ),
                _ => (),
            }
        }
        let lease_for = |ownership: &String, lease: &str| -> Result<()> {
            let owner = self
                .ownership
                .iter()
                .find(|r| &r.ownership_id == ownership)
                .ok_or_else(|| anyhow::anyhow!("integration record has no original owner"))?;
            ensure!(
                matches!(&owner.ownership,NativeOwnership::Acquired{lease:original,..} if original==lease),
                "integration record original lease mismatch"
            );
            Ok(())
        };
        let mut actions = BTreeSet::new();
        for op in &self.operations {
            op.validate()?;
            lease_for(&op.ownership_id, &op.lease)?;
            ensure!(
                actions.insert((&op.lease, &op.operation_id)),
                "duplicate core action"
            );
        }
        for action in &self.integration_actions {
            action.validate_identity()?;
            lease_for(&action.ownership_id, &action.lease)?;
            hex(&action.operation, 32)?;
            digest(&action.request_digest)?;
            digest(&action.prior_binding_digest)?;
            action.slot.validate()?;
            if let IntegrationRequest::Desktop { request, mode, .. } = &action.request {
                ensure!(
                    self.requirements
                        .desktop
                        .as_ref()
                        .is_some_and(|d| d.resource == action.slot && d.modes == [*mode]),
                    "desktop action slot/mode undeclared"
                );
                let binding = self
                    .broker_bindings
                    .iter()
                    .find(|b| {
                        b.ownership_id == action.ownership_id
                            && b.helper_lease == action.lease
                            && b.slot == action.slot
                            && b.broker_lease.as_ref() == Some(&request.broker_lease)
                            && b.publication_scope.is_some()
                            && matches!(b.disposition, Disposition::Complete { .. })
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!("desktop action lacks exact durable broker mapping")
                    })?;
                ensure!(
                    request.binding_digest == binding.expected_header_digest,
                    "desktop action broker header changed"
                );
                if matches!(
                    request.action,
                    theseus_native_protocol::v2::broker::DesktopAction::RequestClose { .. }
                ) {
                    ensure!(self.integration_actions.iter().any(|a|matches!(&a.request,IntegrationRequest::Desktop {request:activation,..} if activation.close_request().as_ref()==Ok(request.as_ref()))),"close differs from precommitted original activation policy");
                }
            }
            action.completion.validate()?;
            ensure!(
                actions.insert((&action.lease, &action.operation)),
                "duplicate integration action"
            );
        }
        for resource in &self.integration_ownership {
            ensure!(
                owners.contains(&resource.key.ownership_id),
                "integration resource lacks owner"
            );
            resource.key.slot.validate()?;
            ensure!(
                resource.manifest_digest == self.manifest_digest,
                "integration resource full manifest mismatch"
            );
            digest(&resource.current_binding_digest)?;
            resource.disposition.validate()?;
        }
        ensure!(
            self.integration_actions.len() + self.operations.len()
                <= 128 * self.ownership.len().max(1),
            "integration operation bound exceeded"
        );
        for action in &self.integration_actions {
            ensure!(
                theseus_native_protocol::v2::request_digest(&action.request.wire()?)?.to_hex()
                    == action.request_digest,
                "integration canonical request digest differs"
            );
            ensure!(
                self.integration_ownership.iter().any(|r| r.key.ownership_id
                    == action.ownership_id
                    && r.key.slot == action.slot
                    && r.binding_history
                        .iter()
                        .any(|b| b.digest == action.prior_binding_digest)),
                "integration action lacks original binding history"
            );
            match (&action.completion, &action.terminal) {
                (
                    Disposition::Complete { receipt: a },
                    Some(IntegrationOutcome::Complete { receipt: b }),
                ) => ensure!(a == b, "integration terminal receipt changed"),
                (
                    Disposition::Failed { reason: a },
                    Some(IntegrationOutcome::Failed { reason: b }),
                ) => ensure!(a == b, "integration terminal failure changed"),
                (Disposition::Unknown { .. }, None | Some(IntegrationOutcome::Unknown { .. })) => {}
                _ => anyhow::bail!("integration terminal disposition pairing differs"),
            }
        }
        let mut resource_keys = BTreeSet::new();
        for resource in &self.integration_ownership {
            ensure!(
                resource_keys.insert((
                    &resource.key.scenario_id,
                    &resource.key.ownership_id,
                    &resource.key.slot
                )),
                "duplicate integration resource key"
            );
            ensure!(
                resource.binding_history.len() <= 64
                    && !resource.binding_history.is_empty()
                    && resource.binding_history.first() == Some(&resource.initial_binding)
                    && resource
                        .binding_history
                        .last()
                        .is_some_and(|b| b.digest == resource.current_binding_digest),
                "integration binding history incomplete"
            );
            for binding in &resource.binding_history {
                digest(&binding.digest)?;
                digest(&binding.artifact_commitment)?;
                digest(&binding.namespace_or_session_receipt_digest)?;
                ensure!(
                    !binding.evidence_ids.is_empty() && binding.evidence_ids.len() <= 64,
                    "binding evidence missing"
                );
                if let Some(original) = &binding.original_action {
                    hex(original, 32)?;
                    ensure!(
                        self.operations.iter().any(|o| o.operation_id == *original
                            && o.ownership_id == resource.key.ownership_id)
                            || self
                                .integration_actions
                                .iter()
                                .any(|a| a.operation == *original
                                    && a.ownership_id == resource.key.ownership_id),
                        "binding original action missing"
                    );
                }
            }
        }
        ensure!(
            self.readiness_probes.len() <= 64 * self.ownership.len().max(1),
            "too many readiness probes"
        );
        let mut probe_ids = BTreeSet::new();
        for probe in &self.readiness_probes {
            let challenge = &probe.challenge;
            if !self.broker_bindings.iter().any(|b| {
                b.ownership_id == challenge.ownership_id
                    && b.broker_lease.as_ref() == Some(&challenge.lease)
                    && matches!(b.disposition, Disposition::Complete { .. })
            }) {
                lease_for(&challenge.ownership_id, &challenge.lease)?;
            }
            challenge.slot.validate()?;
            hex(&challenge.operation, 32)?;
            hex(&challenge.nonce, 32)?;
            challenge.digest()?;
            ensure!(
                (1..=30_000).contains(&challenge.deadline_ms)
                    && probe_ids.insert((&challenge.lease, &challenge.nonce)),
                "duplicate or invalid readiness challenge"
            );
            ensure!(
                probe.expectation.digest()? == probe.expectation_digest
                    && challenge.expectation_digest == probe.expectation_digest,
                "readiness expectation digest mismatch"
            );
            ensure!(
                self.integration_ownership.iter().any(|r| r.key.ownership_id
                    == challenge.ownership_id
                    && r.key.slot == challenge.slot
                    && r.binding_history
                        .iter()
                        .any(|b| b.digest == challenge.resource_binding_digest)),
                "readiness original resource binding missing"
            );
            let rehearsal_start = self.integration_actions.iter().any(|a| {
                a.ownership_id == challenge.ownership_id
                    && a.lease == challenge.lease
                    && a.slot == challenge.slot
                    && a.operation == challenge.operation
                    && a.request_digest == challenge.action_digest
                    && matches!(
                        a.request,
                        IntegrationRequest::Service {
                            control: ServiceAction::Start,
                            ..
                        }
                    )
                    && matches!(a.completion, Disposition::Complete { .. })
            });
            let sut_activation = self.operations.iter().any(|operation| {
                operation.ownership_id == challenge.ownership_id
                    && operation.lease == challenge.lease
                    && operation.operation_id == challenge.operation
                    && operation.request_digest == challenge.action_digest
                    && matches!(operation.completion, Disposition::Complete { .. })
                    && probe.expectation.role == DefinitionRole::Configured
                    && match operation.command.program {
                        CatalogProgram::Configure => {
                            self.requirements.service.as_ref().is_some_and(|s| {
                                s.resource == challenge.slot
                                    && s.controls.configured_activation == ActivationCause::Sut
                            })
                        }
                        CatalogProgram::InstallTarget => {
                            probe.expectation.artifact == ArtifactRole::Target
                                && self.selected_scenarios.iter().any(|s| {
                                    matches!(s, ScenarioKind::Upgrade | ScenarioKind::Reinstall)
                                })
                        }
                        _ => false,
                    }
                    && self.integration_ownership.iter().any(|resource| {
                        resource.key.ownership_id == challenge.ownership_id
                            && resource.key.slot == challenge.slot
                            && resource.binding_history.iter().any(|binding| {
                                binding.digest == challenge.resource_binding_digest
                                    && binding.original_action.as_ref()
                                        == Some(&challenge.operation)
                                    && binding.definition.as_ref()
                                        == Some(&DefinitionKey {
                                            role: probe.expectation.role,
                                            artifact: probe.expectation.artifact,
                                        })
                            })
                    })
            });
            let desktop_activation=self.integration_actions.iter().any(|a|a.ownership_id==challenge.ownership_id && a.operation==challenge.operation
                && a.broker_identity.as_ref().is_some_and(|i|i.broker_lease==challenge.lease && i.request_digest==challenge.action_digest)
                && matches!(&a.request,IntegrationRequest::Desktop {request,..} if request.slot.as_str()==challenge.slot.as_str() && request.prior_binding_digest==challenge.resource_binding_digest && matches!(&request.action,theseus_native_protocol::v2::broker::DesktopAction::Activate {expectation_digest,..} if expectation_digest==&challenge.expectation_digest))
                && matches!(a.completion,Disposition::Complete {..}));
            ensure!(
                rehearsal_start || sut_activation || desktop_activation,
                "readiness lacks exact original Start or declared SUT activation completion"
            );
            probe.completion.validate()?;
            ensure!(
                probe.observation_ids.len() <= 64,
                "too many readiness observations"
            );
            if matches!(probe.completion, Disposition::Complete { .. }) {
                ensure!(
                    !probe.observation_ids.is_empty(),
                    "complete readiness lacks observed response"
                );
            }
            for id in &probe.observation_ids {
                text(id, 128)?;
            }
        }
        ensure!(
            self.broker_bindings.len() <= self.ownership.len(),
            "too many original broker bindings"
        );
        let mut broker_owners = BTreeSet::new();
        for binding in &self.broker_bindings {
            let desktop = self
                .requirements
                .desktop
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("broker binding lacks desktop capability"))?;
            binding.original.validate()?;
            binding.disposition.validate()?;
            let original = &binding.original;
            let header = &original.expected_header;
            lease_for(&binding.ownership_id, &binding.helper_lease)?;
            ensure!(
                broker_owners.insert(&binding.ownership_id)
                    && binding.slot == desktop.resource
                    && original.slot.as_str() == binding.slot.as_str()
                    && original.ownership_id == binding.ownership_id
                    && original.helper_lease == binding.helper_lease
                    && header.manifest_digest == self.manifest_digest
                    && binding.expected_header_digest == header.digest()?.to_hex()
                    && binding.session_id == header.session_id
                    && binding.session_epoch == header.session_epoch
                    && binding.original_intent.intent == original.broker_intent
                    && binding.original_intent.binding == header.binding
                    && binding.original_intent.manifest_digest == header.manifest_digest
                    && binding.original_intent.workspace_marker == header.workspace_marker
                    && binding.original_intent_reference
                        == format!("{}.json", original.broker_intent),
                "broker original binding differs"
            );
            if matches!(binding.disposition, Disposition::Complete { .. }) {
                ensure!(
                    binding.publication_scope.is_some() && binding.broker_lease.is_some(),
                    "broker completion lacks actual publication/mapping"
                );
            }
            if let Some(lease) = &binding.broker_lease {
                hex(lease, 32)?;
                ensure!(
                    binding.publication_scope.is_some(),
                    "broker lease lacks original publication barrier"
                );
            }
        }
        for release in &self.releases {
            lease_for(&release.ownership_id, &release.lease)?;
            release.completion.validate()?;
        }
        for call in &self.provider_calls {
            call.invocation.validate()?;
            call.completion.validate()?;
            if let Some(r) = &call.receipt {
                r.validate()?;
                ensure!(
                    r.manifest_digest == self.manifest_digest,
                    "provider full manifest mismatch"
                );
            }
        }
        // Recovery records import diagnostic knowledge about these exact
        // original actions. They never introduce native authority or a new ID.
        ensure!(
            self.integration_recoveries.len() <= 1024 && self.recovery_attempts.len() <= 1024,
            "integration recovery history exceeds bound"
        );
        let mut core_recoveries = BTreeSet::new();
        for recovery in &self.recovery_attempts {
            hex(&recovery.recovery_id, 32)?;
            ensure!(
                owners.contains(&recovery.ownership_id)
                    && core_recoveries.insert(&recovery.recovery_id),
                "recovery attempt owner missing or identity duplicated"
            );
            recovery.prior_local.validate()?;
            recovery.local.validate()?;
            recovery.remote.validate()?;
        }
        // New records explicitly bind knowledge at publication. For legacy
        // omitted baselines, an original cached query supplies chronology; a
        // later receipt cannot retroactively invalidate earlier Unknown samples.
        let mut broker_terminals = BTreeMap::new();
        for action in self
            .integration_actions
            .iter()
            .filter(|a| matches!(a.request, IntegrationRequest::Desktop { .. }))
        {
            let original = ActionReference {
                operation: action.operation.clone(),
                request_digest: action.request_digest.clone(),
            };
            let first = self
                .integration_recoveries
                .iter()
                .find(|r| r.ownership_id == action.ownership_id);
            let snapshot = first.and_then(|r| {
                r.broker_effect_baselines
                    .iter()
                    .find(|b| b.original == original)
            });
            let recovered_terminal = self
                .integration_recoveries
                .iter()
                .filter(|r| r.ownership_id == action.ownership_id)
                .flat_map(|r| &r.queries)
                .any(|q| q.original == original && q.view == OriginalQueryView::CachedTerminal);
            let initial = if let Some(baseline) = snapshot {
                baseline.terminal.clone()
            } else if recovered_terminal {
                None
            } else {
                action.terminal.clone()
            };
            if let Some(known) = &initial {
                ensure!(
                    action.terminal.as_ref() == Some(known),
                    "initial desktop terminal baseline lacks original action provenance"
                );
            }
            broker_terminals.insert(
                (action.operation.clone(), action.request_digest.clone()),
                initial,
            );
        }
        let mut recovery_ids = BTreeSet::new();
        let mut cached_terminals = BTreeMap::new();
        for (index, recovery) in self.integration_recoveries.iter().enumerate() {
            hex(&recovery.recovery_id, 32)?;
            ensure!(
                recovery_ids.insert(&recovery.recovery_id),
                "duplicate integration recovery"
            );
            let core = self
                .recovery_attempts
                .iter()
                .find(|r| {
                    r.recovery_id == recovery.recovery_id && r.ownership_id == recovery.ownership_id
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("integration recovery lacks published original core attempt")
                })?;
            ensure!(
                !recovery.prior_local.is_empty() && recovery.prior_local.len() <= 4096,
                "integration recovery prior-local evidence missing or excessive"
            );
            for prior in &recovery.prior_local {
                prior.validate()?;
            }
            ensure!(
                recovery.prior_local.contains(&core.prior_local),
                "integration recovery omitted original controller uncertainty"
            );
            recovery.local.validate()?;
            recovery.broker.validate()?;
            recovery.provider.validate()?;
            ensure!(
                !matches!(recovery.local, Disposition::Complete { .. })
                    || recovery.local == core.local,
                "integration recovery local settlement differs from original attempt"
            );
            ensure!(
                !matches!(recovery.provider, Disposition::Complete { .. })
                    || recovery.provider == core.remote,
                "integration recovery provider settlement differs from original attempt"
            );
            for prior in self.integration_recoveries[..index]
                .iter()
                .filter(|r| r.ownership_id == recovery.ownership_id)
            {
                ensure!(
                    recovery.prior_local.contains(&prior.local)
                        && prior
                            .prior_local
                            .iter()
                            .all(|d| recovery.prior_local.contains(d)),
                    "integration recovery erased prior local uncertainty"
                );
            }
            let originals = self
                .integration_actions
                .iter()
                .filter(|a| a.ownership_id == recovery.ownership_id)
                .map(|a| ActionReference {
                    operation: a.operation.clone(),
                    request_digest: a.request_digest.clone(),
                })
                .collect::<Vec<_>>();
            ensure!(
                recovery.original_actions == originals,
                "integration recovery original action set differs"
            );
            if !recovery.broker_effect_baselines.is_empty() {
                let desktops = self
                    .integration_actions
                    .iter()
                    .filter(|a| {
                        a.ownership_id == recovery.ownership_id
                            && matches!(a.request, IntegrationRequest::Desktop { .. })
                    })
                    .collect::<Vec<_>>();
                ensure!(
                    recovery.broker_effect_baselines.len() == desktops.len(),
                    "desktop terminal baseline set differs from original actions"
                );
                for (baseline, action) in recovery.broker_effect_baselines.iter().zip(desktops) {
                    ensure!(
                        baseline.original.operation == action.operation
                            && baseline.original.request_digest == action.request_digest,
                        "desktop terminal baseline identity changed"
                    );
                    ensure!(
                        broker_terminals
                            .get(&(action.operation.clone(), action.request_digest.clone()))
                            == Some(&baseline.terminal),
                        "desktop terminal baseline changed without original cached query provenance"
                    );
                }
            }
            ensure!(
                recovery.broker_observations.len() <= 64 * recovery.original_actions.len().max(1),
                "broker recovery observations exceed bound"
            );
            let mut broker_states = BTreeMap::new();
            for prior in self.integration_recoveries[..index]
                .iter()
                .filter(|r| r.ownership_id == recovery.ownership_id)
            {
                for observation in &prior.broker_observations {
                    broker_states.insert(
                        observation.original.operation.clone(),
                        observation.state.clone(),
                    );
                }
            }
            for observation in &recovery.broker_observations {
                observation.identity.validate()?;
                observation.state.validate()?;
                let action = self
                    .integration_actions
                    .iter()
                    .find(|a| {
                        a.ownership_id == recovery.ownership_id
                            && a.operation == observation.original.operation
                            && a.request_digest == observation.original.request_digest
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!("broker observation lacks original published action")
                    })?;
                let IntegrationRequest::Desktop { request, .. } = &action.request else {
                    anyhow::bail!("broker observation borrowed service authority")
                };
                ensure!(
                    request.identity()? == observation.identity,
                    "broker observation original identity changed"
                );
                crate::native::integration::owned_lifetime::validate_recovered_observation(
                    self,
                    action,
                    &observation.state,
                )?;
                validate_broker_effect_terminal(
                    broker_terminals
                        .get(&(action.operation.clone(), action.request_digest.clone()))
                        .and_then(|t| t.as_ref()),
                    &observation.state,
                )?;
                if observation.source == BrokerObservationSource::Cancel {
                    ensure!(
                        matches!(
                            request.action,
                            theseus_native_protocol::v2::broker::DesktopAction::Activate { .. }
                        ),
                        "broker Cancel must name the original activation owner"
                    );
                }
                if let Some(previous) = broker_states.get(&observation.original.operation) {
                    validate_broker_progress(previous, &observation.state)?;
                }
                broker_states.insert(
                    observation.original.operation.clone(),
                    observation.state.clone(),
                );
            }
            if let Some(release) = &recovery.broker_release {
                let binding = self
                    .broker_bindings
                    .iter()
                    .find(|b| {
                        b.ownership_id == recovery.ownership_id
                            && b.helper_lease == release.helper_lease
                            && b.broker_lease.as_ref() == Some(&release.broker_lease)
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!("broker release lacks exact original mapping")
                    })?;
                let owner = self
                    .ownership
                    .iter()
                    .find(|o| o.ownership_id == binding.ownership_id)
                    .ok_or_else(|| {
                        anyhow::anyhow!("broker release original helper owner missing")
                    })?;
                ensure!(
                    matches!(&owner.ownership,NativeOwnership::Acquired {prepared,lease} if lease==&release.helper_lease && prepared.intent==release.helper_intent),
                    "broker release original helper intent changed"
                );
            }
            let mut queried = BTreeSet::new();
            for query in &recovery.queries {
                hex(&query.original.operation, 32)?;
                digest(&query.original.request_digest)?;
                ensure!(
                    queried.insert((&query.original.operation, &query.original.request_digest)),
                    "duplicate original recovery query"
                );
                let action = self
                    .integration_actions
                    .iter()
                    .find(|a| {
                        a.ownership_id == recovery.ownership_id
                            && a.operation == query.original.operation
                            && a.request_digest == query.original.request_digest
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!("integration recovery query has no exact original action")
                    })?;
                match &query.outcome {
                    IntegrationOutcome::Complete { receipt } => text(receipt, 4096)?,
                    IntegrationOutcome::Unknown { reason }
                    | IntegrationOutcome::Failed { reason } => text(reason, 4096)?,
                }
                match query.view {
                    OriginalQueryView::NonterminalUnknown => ensure!(
                        matches!(query.outcome, IntegrationOutcome::Unknown { .. }),
                        "nonterminal recovery query cannot claim a terminal outcome"
                    ),
                    OriginalQueryView::CachedTerminal => {
                        validate_original_query_broker_receipts(
                            self,
                            &recovery.ownership_id,
                            query,
                        )?;
                        if matches!(action.request, IntegrationRequest::Desktop { .. }) {
                            let known = broker_terminals
                                .get_mut(&(action.operation.clone(), action.request_digest.clone()))
                                .expect("original desktop terminal tracked");
                            if let Some(terminal) = known.as_ref() {
                                ensure!(
                                    terminal == &query.outcome,
                                    "original desktop query changed earlier terminal knowledge"
                                );
                            } else {
                                *known = Some(query.outcome.clone());
                            }
                        }
                        if let Some(prior) = cached_terminals.insert(
                            (
                                &recovery.ownership_id,
                                &query.original.operation,
                                &query.original.request_digest,
                            ),
                            &query.outcome,
                        ) {
                            ensure!(
                                prior == &query.outcome,
                                "original cached recovery terminal changed"
                            );
                        }
                        if let Some(terminal) = &action.terminal {
                            ensure!(
                                terminal == &query.outcome,
                                "integration recovery query changed known cached terminal outcome"
                            );
                        }
                    }
                }
            }
        }
        for action in self
            .integration_actions
            .iter()
            .filter(|a| matches!(a.request, IntegrationRequest::Desktop { .. }))
        {
            if let Some(terminal) = &action.terminal {
                ensure!(
                    broker_terminals
                        .get(&(action.operation.clone(), action.request_digest.clone()))
                        .and_then(|t| t.as_ref())
                        == Some(terminal),
                    "new desktop terminal lacks original cached query provenance"
                );
            }
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
