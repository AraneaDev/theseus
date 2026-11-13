//! Desktop binding, observation recording, inspection and resource publication.
use super::*;

impl<'a, 'b, 's> DesktopDriver<'a, 'b, 's> {
    pub(super) fn bind(&mut self) -> Result<()> {
        self.core.reserve_operation()?;
        let lease = self.core.lease()?;
        let original = broker::BrokerBind {
            ownership_id: self.core.resource.resource().ownership_id.clone(),
            helper_lease: lease.to_hex(),
            helper_operation: operation()?.to_hex(),
            slot: v2::ResourceSlot::new(self.hooks.requirement.resource.as_str())?,
            broker_intent: nonce()?.to_hex(),
            expected_header: self.hooks.header.clone(),
        };
        ensure!(
            original.ownership_id == self.hooks.header.ownership_id,
            "desktop session belongs to another provider ownership"
        );
        let record=BrokerBindingRecord {original:original.clone(),ownership_id:original.ownership_id.clone(),helper_lease:original.helper_lease.clone(),slot:self.hooks.requirement.resource.clone(),original_intent:BrokerIntentRecord {intent:original.broker_intent.clone(),binding:self.hooks.header.binding.clone(),manifest_digest:self.hooks.header.manifest_digest.clone(),workspace_marker:self.hooks.header.workspace_marker.clone()},publication_scope:None,original_intent_reference:format!("{}.json",original.broker_intent),expected_header_digest:self.hooks.header.digest()?.to_hex(),session_id:self.hooks.header.session_id.clone(),session_epoch:self.hooks.header.session_epoch.clone(),broker_lease:None,disposition:Disposition::Unknown {reason:"original broker bind published; no helper acceptance or publication barrier observed".into()}};
        let token = self
            .core
            .projector
            .publish_broker_binding(&self.core.resource, record)?;
        let request = self.core.session.submit(Action::BrokerBind {
            original: token.into_original(),
        })?;
        let v2::Reply::BrokerBinding { receipt } = self.core.wait_reply(request)? else {
            bail!("original broker bind unresolved")
        };
        ensure!(receipt.original == original, "broker changed original bind");
        self.core.projector.record_broker_binding(&receipt)?;
        self.hooks.receipt = Some(*receipt);
        let slot = self.hooks.requirement.resource.clone();
        let service_declared = self.hooks.manifest.service_authority.is_some();
        let evidence = self.evidence()?;
        evidence.desktops.push(DesktopEvidence {
            slot,
            observations: vec![],
            actions: vec![],
        });
        if !service_declared {
            evidence.dispositions.manager_action = Disposition::Complete {
                receipt: "no service manager authority declared".into(),
            };
        }
        evidence.dispositions.broker = Disposition::Unknown {
            reason: "original broker lease remains held".into(),
        };
        Ok(())
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
    pub(super) fn binding(
        &self,
        phase: IntegrationPhase,
        digest: String,
        complete: bool,
        action: Option<String>,
    ) -> Result<ObservationBinding> {
        let owner = self.core.resource.resource();
        let StoredRun::NativeIntegration(report) = self.core.projector.report() else {
            bail!("schema2 report missing")
        };
        Ok(ObservationBinding {
            id: format!("desktop-{}-{}", owner.scenario_id, self.hooks.sequence),
            run_id: report.run_id.clone(),
            scenario_id: owner.scenario_id.clone(),
            ownership_id: owner.ownership_id.clone(),
            lease: self.core.lease()?.to_hex(),
            slot: self.hooks.requirement.resource.clone(),
            request_id: format!("desktop-observe-{}", self.hooks.sequence),
            action,
            binding_digest: digest,
            source_profile: super::super::PROFILE.into(),
            source_version: "authenticated_original_desktop_broker_v1".into(),
            phase,
            definition: if matches!(
                phase,
                IntegrationPhase::Baseline | IntegrationPhase::Uninstalled
            ) {
                None
            } else {
                self.hooks
                    .definition
                    .as_ref()
                    .map(|d| DefinitionKey::from_desktop_wire(d.key))
            },
            sequence: self.hooks.sequence,
            started_ms: self.hooks.started.elapsed().as_millis() as u64,
            finished_ms: self.hooks.started.elapsed().as_millis() as u64,
            complete,
        })
    }
    pub(super) fn record_observation(&mut self, bound: BoundDesktopObservation) -> Result<()> {
        if let Some(next) = &bound.broker_state {
            for previous in self.evidence()?.desktops[0]
                .observations
                .iter()
                .filter(|o| o.binding.action == bound.binding.action)
                .filter_map(|o| o.broker_state.as_ref())
            {
                validate_broker_progress(previous, next)?;
            }
        }
        let specs = desktop_scenario_check_specs(&self.hooks.requirement, self.hooks.scenario);
        let scenario = self.core.resource.resource().scenario_id.clone();
        let sut = self.sut_operation();
        for (id, phase, predicate, origin) in specs.into_iter().filter(|(_, p, predicate, _)| {
            *p == bound.binding.phase
                && match predicate {
                    IntegrationPredicate::ApplicationReady => bound.readiness.is_some(),
                    IntegrationPredicate::Lifetime(_) => {
                        bound.broker_state.is_some() && bound.readiness.is_none()
                    }
                    _ => bound.broker_state.is_none() && bound.readiness.is_none(),
                }
        }) {
            let outcome = if !bound.binding.complete {
                CheckOutcome::Incomplete
            } else if desktop_predicate_satisfied(&bound, &predicate) {
                CheckOutcome::Passed
            } else if matches!(predicate, IntegrationPredicate::Lifetime(_)) {
                CheckOutcome::Incomplete
            } else {
                CheckOutcome::Failed
            };
            if outcome != CheckOutcome::Passed {
                self.hooks.forward = false;
            }
            let slot = self.hooks.requirement.resource.clone();
            self.evidence()?.checks.push(IntegrationCheck {
                id,
                scenario_id: scenario.clone(),
                phase,
                slot,
                role: bound.binding.definition.as_ref().map(|d| d.role),
                artifact: bound.binding.definition.as_ref().map(|d| d.artifact),
                predicate,
                origin,
                sut_operation: if origin == CheckOrigin::SutPostcondition {
                    sut.clone()
                } else {
                    None
                },
                observation_ids: vec![bound.binding.id.clone()],
                outcome,
                detail: match outcome {
                    CheckOutcome::Passed => {
                        "original desktop facts satisfy this independent predicate"
                    }
                    CheckOutcome::Failed => {
                        "original desktop facts contradict the required predicate"
                    }
                    _ => "original desktop fact remains unproved",
                }
                .into(),
            });
        }
        self.evidence()?.desktops[0].observations.push(bound);
        self.core.projector.save_report()
    }
    pub(super) fn inspect(&mut self, phase: IntegrationPhase) -> Result<()> {
        let lease = self.core.lease()?;
        let id = self.core.session.submit(Action::InspectDesktop {
            lease,
            slot: v2::ResourceSlot::new(self.hooks.requirement.resource.as_str())?,
        })?;
        let reply = self.core.wait_reply(id)?;
        let (observed, observation) = match reply {
            v2::Reply::DesktopObservation { lease, observation } => (lease, observation),
            v2::Reply::Refused { code, .. } => bail!("desktop entry inspection refused: {code:?}"),
            _ => bail!("desktop entry inspection unavailable"),
        };
        ensure!(
            observed == lease
                && observation.slot.as_str() == self.hooks.requirement.resource.as_str()
                && observation.session_header_digest == self.hooks.header.digest()?.to_hex(),
            "desktop entry original binding mismatch"
        );
        if !self.hooks.manifest.desktop_definitions.is_empty()
            && !matches!(
                phase,
                IntegrationPhase::Baseline | IntegrationPhase::Uninstalled
            )
        {
            let generation = observation.generation.as_ref().ok_or_else(|| {
                anyhow::anyhow!("desktop inspection missing original SUT generation")
            })?;
            let key = desktop_phase_definition(self.hooks.scenario, phase)
                .unwrap()
                .desktop_wire();
            let tuple = self.hooks.manifest.desktop_definition(&key)?;
            tuple.validate()?;
            ensure!(
                generation.definition == key
                    && generation.tuple_digest == tuple.digest()?.to_hex()
                    && self.sut_operation().as_ref() == Some(&generation.sut.operation),
                "desktop inspection changed exact SUT generation/tuple"
            );
            self.hooks.definition = Some(tuple);
        } else {
            ensure!(
                observation.generation.is_none(),
                "legacy/absent desktop inspection invented a generation"
            );
        }
        self.hooks.sequence += 1;
        let binding = self.binding(
            phase,
            observation.binding_digest.clone(),
            observation.complete,
            self.sut_operation(),
        )?;
        let expected = self.desktop_manifest();
        let entry_valid = observation.entry_valid
            && observation.entry_digest.as_ref() == Some(&expected.entry_digest)
            && observation.executable_digest.as_ref() == Some(&expected.executable_digest);
        let bound = BoundDesktopObservation {
            generation: observation
                .generation
                .as_ref()
                .map(desktop_generation_evidence),
            entry_valid: Some(entry_valid),
            broker_state: None,
            readiness: None,
            evidence: None,
            binding: binding.clone(),
            entry_present: IntegrationProvenance::Observed {
                value: observation.entry_present,
                source: "retained nofollow desktop entry and independent artifact commitment"
                    .into(),
            },
            activated: IntegrationProvenance::Unavailable {
                reason: "entry presence proves no activation".into(),
            },
            primary_exited: IntegrationProvenance::Unavailable {
                reason: "entry inspection has no process authority".into(),
            },
            descendant_scope_settled: IntegrationProvenance::Unavailable {
                reason: "entry inspection has no descendant authority".into(),
            },
        };
        if let (StoredRun::NativeIntegration(report), StoredJournal::NativeIntegration(journal)) =
            (self.core.projector.report(), self.core.projector.journal())
        {
            validate_desktop_observation_generation(report, journal, &bound)?;
        }
        self.hooks.entry = Some(*observation);
        self.record_observation(bound)?;
        if !matches!(
            phase,
            IntegrationPhase::Baseline | IntegrationPhase::Uninstalled
        ) {
            self.publish_resource(&binding)?;
        }
        if phase == IntegrationPhase::Uninstalled {
            let desktop = if !self.hooks.entry.as_ref().unwrap().complete {
                Disposition::Unknown {
                    reason: "desktop SUT removal observation incomplete".into(),
                }
            } else if self.hooks.entry.as_ref().unwrap().entry_present {
                Disposition::Failed {
                    reason: "SUT left the declared desktop entry".into(),
                }
            } else {
                Disposition::Complete {
                    receipt: "SUT removed original desktop entry before disposal".into(),
                }
            };
            let combined = self.hooks.manifest.service_authority.is_some();
            let evidence = self.evidence()?;
            evidence.dispositions.registration = registration_removal_disposition(
                combined.then_some(&evidence.dispositions.registration),
                desktop,
            );
        }
        if phase == IntegrationPhase::Uninstalled
            && self
                .hooks
                .entry
                .as_ref()
                .is_some_and(|e| e.complete && !e.entry_present)
        {
            if self
                .hooks
                .state
                .as_ref()
                .is_some_and(settled_original_broker_state)
            {
                let StoredJournal::NativeIntegration(journal) = self.core.projector.journal()
                else {
                    bail!("schema2 journal missing")
                };
                if let Some(mut record) = journal
                    .integration_ownership
                    .iter()
                    .find(|r| {
                        r.key.ownership_id == self.core.resource.resource().ownership_id
                            && r.key.slot == self.hooks.requirement.resource
                    })
                    .cloned()
                {
                    record.disposition = Disposition::Complete {
                        receipt: "original desktop native scope/IPC settled and SUT entry removed"
                            .into(),
                    };
                    self.core.projector.publish_integration_resource(record)?;
                }
            }
            self.core.projector.save_report()?;
        }
        Ok(())
    }
    fn publish_resource(&mut self, binding: &ObservationBinding) -> Result<()> {
        let snapshot = ResourceBindingSnapshot {
            digest: binding.binding_digest.clone(),
            basis: BindingBasis::AdmittedActivation,
            evidence_ids: vec![binding.id.clone()],
            definition: binding.definition.clone(),
            artifact_commitment: self.executable_manifest().sha256.clone(),
            namespace_or_session_receipt_digest: self.hooks.header.digest()?.to_hex(),
            original_action: if self
                .hooks
                .entry
                .as_ref()
                .is_some_and(|e| e.generation.is_some())
            {
                binding.action.clone()
            } else {
                None
            },
        };
        if let StoredJournal::NativeIntegration(journal) = self.core.projector.journal()
            && let Some(mut record) = journal
                .integration_ownership
                .iter()
                .find(|r| r.key.ownership_id == binding.ownership_id && r.key.slot == binding.slot)
                .cloned()
        {
            ensure!(
                record.revocation.is_none(),
                "desktop generation cannot restore revoked original roots"
            );
            if record.current_binding_digest != binding.binding_digest {
                record.binding_history.push(snapshot);
                record.current_binding_digest = binding.binding_digest.clone();
                record.disposition = Disposition::Unknown {
                    reason: "fresh original SUT generation admitted; lifetime unproved".into(),
                };
                self.core.projector.publish_integration_resource(record)?;
            }
            return Ok(());
        }
        let record = IntegrationResourceRecord {
            key: IntegrationResourceKey {
                scenario_id: binding.scenario_id.clone(),
                ownership_id: binding.ownership_id.clone(),
                slot: binding.slot.clone(),
            },
            kind: IntegrationResourceKind::Desktop,
            manifest_digest: self.hooks.header.manifest_digest.clone(),
            initial_binding: snapshot.clone(),
            binding_history: vec![snapshot],
            current_binding_digest: binding.binding_digest.clone(),
            revocation: None,
            disposition: Disposition::Unknown {
                reason: "original entry/session admitted; original runtime unsettled".into(),
            },
        };
        self.core.projector.publish_integration_resource(record)
    }
}
