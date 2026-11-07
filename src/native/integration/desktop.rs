//! Desktop hooks compose the same ordinary lifecycle and retained projector.
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
use theseus_native_client::Action;
use theseus_native_protocol::v2::{self, broker};

pub struct DesktopDriver<'a, 'b, 's> {
    core: &'a mut NativeDriver<'b, 's>,
    hooks: DesktopHooks,
}
pub(crate) struct DesktopHooks {
    header: broker::IntegrationBrokerHeader,
    manifest: v2::IntegrationManifest,
    requirement: DesktopRequirement,
    mode: broker::DesktopMode,
    receipt: Option<broker::BrokerBindingReceipt>,
    entry: Option<broker::DesktopObservation>,
    activation: Option<broker::DesktopRequest>,
    close: Option<broker::DesktopRequest>,
    state: Option<broker::BrokerState>,
    sequence: u32,
    started: Instant,
    forward: bool,
    deferred_failure: Option<anyhow::Error>,
    scenario: ScenarioKind,
    first_install: bool,
    after_reinstall: bool,
    definition: Option<v2::DesktopDefinition>,
}
impl DesktopHooks {
    pub(crate) fn forward_active(&self) -> bool {
        self.forward
    }
}

fn desktop_readiness_expectation(
    expected: &v2::ReadinessDefinitionExpectation,
) -> Result<ReadinessExpectation> {
    expected.validate()?;
    let role = match expected.role.as_str() {
        "base" => DefinitionRole::Base,
        "configured" => DefinitionRole::Configured,
        "target" => DefinitionRole::Target,
        _ => bail!("unsupported desktop readiness definition role"),
    };
    let artifact = match expected.artifact.as_str() {
        "base" => ArtifactRole::Base,
        "target" => ArtifactRole::Target,
        _ => bail!("unsupported desktop readiness artifact role"),
    };
    let expectation = ReadinessExpectation {
        role,
        artifact,
        artifact_length: expected.artifact_length,
        artifact_digest: expected.artifact_sha256.clone(),
        version: expected.version_literal.clone(),
        data_digest: expected.data_sha256.clone(),
        configuration_digest: expected.configuration_digest.clone(),
        initialized: true,
    };
    ensure!(
        expectation.digest()? == expected.digest()?.to_hex(),
        "desktop readiness canonical expectation changed"
    );
    Ok(expectation)
}
pub(crate) fn desktop_readiness_completion(observation: &BoundDesktopObservation) -> Disposition {
    if desktop_predicate_satisfied(observation, &IntegrationPredicate::ApplicationReady)
        && let Some(receipt) = observation
            .readiness
            .as_ref()
            .and_then(|r| r.response_digest.as_ref())
    {
        return Disposition::Complete {
            receipt: receipt.clone(),
        };
    }
    Disposition::Unknown {
        reason: "original desktop readiness unproved".into(),
    }
}
fn registration_removal_disposition(
    service: Option<&Disposition>,
    desktop: Disposition,
) -> Disposition {
    match service {
        Some(service) if !matches!(service, Disposition::Complete { .. }) => service.clone(),
        _ => desktop,
    }
}
fn selected_desktop_mode(
    modes: &[DesktopMode],
    lifetime: LifetimeRequirement,
    native: Option<&v2::NativeDesktopDefinition>,
) -> Result<broker::DesktopMode> {
    match (modes, native) {
        ([DesktopMode::OwnedExecutable], None)
            if lifetime != LifetimeRequirement::ActivationInstanceTerminated =>
        {
            Ok(broker::DesktopMode::OwnedExecutable)
        }
        ([DesktopMode::NativeActivation], Some(definition))
            if lifetime.native() == definition.lifetime()
                && lifetime != LifetimeRequirement::OwnedProcessTreeSettled =>
        {
            Ok(broker::DesktopMode::NativeActivation)
        }
        _ => bail!("selected desktop mode/lifetime differs from exact admitted native capability"),
    }
}
/// Pure exact-description admission, called before preparing an intent/Acquire.
pub(crate) fn validate_admission(
    recipe: &ValidatedIntegrationRecipe,
    manifest: &v2::IntegrationManifest,
    header: &broker::IntegrationBrokerHeader,
    scenario: ScenarioKind,
) -> Result<broker::DesktopMode> {
    manifest.validate()?;
    header.validate()?;
    let requirement = recipe
        .recipe
        .integrations
        .desktop
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("desktop requirement missing"))?;
    let desktop = manifest
        .desktop
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("desktop capability missing"))?;
    let mode = selected_desktop_mode(
        &requirement.modes,
        requirement.lifetime,
        manifest.desktop_native.as_ref(),
    )?;
    ensure!(
        desktop.slot.as_str() == requirement.resource.as_str()
            && desktop.allowed_modes == [mode]
            && header.allowed_modes == [mode],
        "desktop authority/requirements differ"
    );
    if let Some(entry) = &manifest.desktop_owned_entry {
        ensure!(
            mode == broker::DesktopMode::OwnedExecutable && entry.os() == header.os,
            "owned native entry differs from selected mode/platform"
        );
    }
    if let Some(owned) = manifest.desktop_owned_lifetime {
        owned.validate_platform(&header.os)?;
        ensure!(
            mode == broker::DesktopMode::OwnedExecutable
                && requirement.lifetime == LifetimeRequirement::OwnedPrimaryExited,
            "owned lifetime differs from the selected exact requirement"
        );
    } else {
        ensure!(
            header.os != "macos" || mode != broker::DesktopMode::OwnedExecutable,
            "OwnedExecutable lifetime on macOS requires the committed original-primary declaration; choose NativeActivation for manager-instance termination"
        );
    }
    #[cfg(feature = "native-integration-test-support")]
    let source_headless = header.display == broker::SuppliedDisplay::SourceOwnedHeadless;
    #[cfg(not(feature = "native-integration-test-support"))]
    let source_headless = false;
    ensure!(
        mode != broker::DesktopMode::OwnedExecutable
            || header.os == "linux"
            || source_headless
            || manifest.desktop_owned_entry.is_some(),
        "interactive owned execution requires exact native entry declaration before acquisition"
    );
    if let Some(native) = &manifest.desktop_native {
        ensure!(
            native.os() == header.os,
            "native desktop platform differs from original broker"
        );
        // This pure description check grants no Linux authority. The original
        // session must also retain its authenticated provider/helper admission
        // before Acquire and again at driver construction.
    }
    let expected = manifest
        .desktop_readiness
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("desktop readiness manifest missing"))?;
    ensure!(
        expected.artifact_length == recipe.core.recipe.target.length
            && expected.artifact_sha256 == recipe.core.recipe.target.sha256,
        "desktop executable differs from original admitted target artifact"
    );
    desktop_readiness_expectation(expected)?;
    if scenario != ScenarioKind::Clean || !manifest.desktop_definitions.is_empty() {
        ensure!(
            !manifest.desktop_definitions.is_empty(),
            "ordinary desktop transition requires precommitted per-definition tuples"
        );
        let phases = if scenario == ScenarioKind::Upgrade {
            vec![
                IntegrationPhase::BaseInstalled,
                IntegrationPhase::Configured,
                IntegrationPhase::TargetInstalledAfterUpgrade,
            ]
        } else {
            vec![
                IntegrationPhase::TargetInstalledInitial,
                IntegrationPhase::Configured,
            ]
        };
        for phase in phases {
            let key = desktop_phase_definition(scenario, phase)
                .unwrap()
                .desktop_wire();
            let tuple = manifest.desktop_definition(&key)?;
            tuple.validate()?;
            ensure!(
                selected_desktop_mode(
                    &requirement.modes,
                    requirement.lifetime,
                    tuple.native.as_ref()
                )? == mode,
                "desktop tuple changes admitted mode/lifetime"
            );
            let artifact = if key.artifact == v2::DesktopArtifactRole::Base {
                &recipe.core.recipe.base
            } else {
                &recipe.core.recipe.target
            };
            ensure!(
                tuple.executable.length == artifact.length
                    && tuple.executable.sha256 == artifact.sha256,
                "desktop tuple differs from original admitted reader artifact"
            );
        }
    }
    Ok(mode)
}
fn nonce() -> Result<v2::BindingId> {
    let mut bytes = [0; 16];
    theseus_host::random_bytes(&mut bytes)?;
    ensure!(bytes != [0; 16], "random desktop nonce is zero");
    Ok(v2::BindingId::from_bytes(bytes))
}
fn operation() -> Result<v2::OperationId> {
    Ok(v2::OperationId::from_bytes(*nonce()?.as_bytes()))
}
impl<'a, 'b, 's> DesktopDriver<'a, 'b, 's> {
    pub fn new(
        core: &'a mut NativeDriver<'b, 's>,
        recipe: &ValidatedIntegrationRecipe,
        header: broker::IntegrationBrokerHeader,
    ) -> Result<Self> {
        Self::new_for_scenario(core, recipe, header, ScenarioKind::Clean)
    }
    pub(crate) fn new_for_scenario(
        core: &'a mut NativeDriver<'b, 's>,
        recipe: &ValidatedIntegrationRecipe,
        header: broker::IntegrationBrokerHeader,
        scenario: ScenarioKind,
    ) -> Result<Self> {
        let manifest = core
            .session
            .integration_description()
            .ok_or_else(|| anyhow::anyhow!("desktop helper description missing"))?
            .manifest
            .as_ref()
            .clone();
        let requirement = recipe
            .recipe
            .integrations
            .desktop
            .clone()
            .ok_or_else(|| anyhow::anyhow!("desktop requirements missing"))?;
        let mode = validate_admission(recipe, &manifest, &header, scenario)?;
        core.session
            .require_linux_gio_admission(&manifest, &header)?;
        Ok(Self {
            core,
            hooks: DesktopHooks {
                header,
                manifest,
                requirement,
                mode,
                receipt: None,
                entry: None,
                activation: None,
                close: None,
                state: None,
                sequence: 0,
                started: Instant::now(),
                forward: true,
                deferred_failure: None,
                scenario,
                first_install: true,
                after_reinstall: false,
                definition: None,
            },
        })
    }
    fn desktop_manifest(&self) -> &broker::DesktopManifest {
        self.hooks
            .definition
            .as_ref()
            .map(|d| &d.desktop)
            .unwrap_or_else(|| self.hooks.manifest.desktop.as_ref().unwrap())
    }
    fn readiness_manifest(&self) -> &v2::ReadinessDefinitionExpectation {
        self.hooks
            .definition
            .as_ref()
            .map(|d| &d.readiness)
            .unwrap_or_else(|| self.hooks.manifest.desktop_readiness.as_ref().unwrap())
    }
    fn executable_manifest(&self) -> &v2::DesktopExecutable {
        self.hooks
            .definition
            .as_ref()
            .map(|d| &d.executable)
            .unwrap_or_else(|| self.hooks.manifest.desktop_executable.as_ref().unwrap())
    }
    pub(crate) fn resume(core: &'a mut NativeDriver<'b, 's>, hooks: DesktopHooks) -> Self {
        Self { core, hooks }
    }
    pub(crate) fn into_hooks(self) -> DesktopHooks {
        self.hooks
    }
    pub(crate) fn prepare_hooks(&mut self, scenario: ScenarioKind) -> Result<()> {
        ensure!(
            scenario == self.hooks.scenario,
            "desktop scenario differs from exact admission"
        );
        self.bind()?;
        self.inspect(IntegrationPhase::Baseline)
    }
    pub(crate) fn finish_hooks(&mut self, result: Result<()>) -> Result<()> {
        match (self.hooks.deferred_failure.take(), result) {
            (None, result) => result,
            (Some(primary), Ok(())) => Err(primary),
            (Some(primary), Err(secondary)) => Err(anyhow::Error::new(DesktopCleanupFailure {
                primary,
                secondary,
            })),
        }
    }
    pub fn run(&mut self, scenario: ScenarioKind) -> Result<()> {
        ensure!(
            scenario == self.hooks.scenario,
            "desktop scenario differs from exact admission"
        );
        self.core.prepare(scenario)?;
        self.prepare_hooks(scenario)?;
        let result = common::run_common(self, scenario);
        self.finish_hooks(result)
    }
    fn evidence(&mut self) -> Result<&mut IntegrationScenarioEvidence> {
        let id = self.core.resource.resource().ownership_id.clone();
        let StoredRun::NativeIntegration(report) = self.core.projector.report_mut() else {
            bail!("desktop requires schema2 report")
        };
        report
            .integrations
            .iter_mut()
            .find(|s| s.ownership_id == id)
            .ok_or_else(|| anyhow::anyhow!("desktop scenario missing"))
    }
    fn bind(&mut self) -> Result<()> {
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
    fn binding(
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
            source_profile: super::PROFILE.into(),
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
    fn record_observation(&mut self, bound: BoundDesktopObservation) -> Result<()> {
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
    fn inspect(&mut self, phase: IntegrationPhase) -> Result<()> {
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
    fn dispatch_action(&mut self, request: broker::DesktopRequest) -> Result<broker::BrokerState> {
        self.core.reserve_operation()?;
        let wire = v2::Request::DesktopAction {
            request: Box::new(request.clone()),
        };
        let identity = request.identity()?;
        let record = IntegrationActionRecord {
            ownership_id: self.core.resource.resource().ownership_id.clone(),
            lease: request.helper_lease.clone(),
            slot: self.hooks.requirement.resource.clone(),
            operation: request.helper_operation.clone(),
            request: IntegrationRequest::Desktop {
                lease: request.helper_lease.clone(),
                operation: request.helper_operation.clone(),
                slot: self.hooks.requirement.resource.clone(),
                prior_binding_digest: request.prior_binding_digest.clone(),
                mode: if self.hooks.mode == broker::DesktopMode::NativeActivation {
                    DesktopMode::NativeActivation
                } else {
                    DesktopMode::OwnedExecutable
                },
                request: Box::new(request.clone()),
            },
            request_digest: v2::request_digest(&wire)?.to_hex(),
            prior_binding_digest: request.prior_binding_digest.clone(),
            broker_identity: Some(serde_json::from_value(serde_json::to_value(&identity)?)?),
            completion: Disposition::Unknown {
                reason: "exact original desktop action published; effect unproved".into(),
            },
            terminal: None,
        };
        let token = self
            .core
            .projector
            .publish_integration_action(&self.core.resource, record)?;
        let saved = token.into_action();
        let id = self.core.session.submit(Action::DesktopAction {
            request: request.clone(),
        })?;
        let v2::Reply::DesktopState {
            identity: observed,
            state,
        } = self.core.wait_reply(id)?
        else {
            bail!("original desktop action unresolved")
        };
        ensure!(
            *observed == identity,
            "desktop original action identity changed"
        );
        self.validate_state(&state)?;
        let terminal = match &state.effect {
            broker::BrokerEffect::Receipt { receipt } if receipt.acknowledged => {
                IntegrationOutcome::Complete {
                    receipt: receipt.receipt_digest.clone(),
                }
            }
            broker::BrokerEffect::Receipt { .. } => IntegrationOutcome::Failed {
                reason: "original desktop effect was not acknowledged".into(),
            },
            broker::BrokerEffect::Unknown => IntegrationOutcome::Unknown {
                reason: "original broker effect unproved".into(),
            },
        };
        let completion = match &terminal {
            IntegrationOutcome::Complete { receipt } => Disposition::Complete {
                receipt: receipt.clone(),
            },
            IntegrationOutcome::Failed { reason } => Disposition::Failed {
                reason: reason.clone(),
            },
            IntegrationOutcome::Unknown { reason } => Disposition::Unknown {
                reason: reason.clone(),
            },
        };
        if !matches!(terminal, IntegrationOutcome::Unknown { .. }) {
            self.core.projector.record_integration_completion(
                &request.helper_lease,
                &request.helper_operation,
                completion,
                terminal,
            )?;
        }
        self.evidence()?.desktops[0].actions.push(ActionReference {
            operation: saved.operation,
            request_digest: saved.request_digest,
        });
        self.core.projector.save_report()?;
        Ok(*state)
    }
    fn activate(&mut self) -> Result<()> {
        let transition = self.hooks.entry.as_ref().and_then(|e| e.generation.clone());
        if !self.hooks.manifest.desktop_definitions.is_empty() {
            ensure!(
                transition.is_some(),
                "desktop generation transition missing"
            );
        }
        if let Some(previous) = &self.hooks.activation {
            ensure!(
                self.hooks
                    .state
                    .as_ref()
                    .is_some_and(settled_original_broker_state),
                "desktop generation cannot replace unsettled original owner"
            );
            ensure!(
                transition
                    .as_ref()
                    .and_then(|t| t.previous_activation.as_ref())
                    == Some(&previous.identity()?),
                "desktop generation changed retired original identity"
            );
        } else if let Some(transition) = &transition {
            ensure!(
                transition.previous_activation.is_none(),
                "initial desktop generation borrowed a prior activation"
            );
        }
        let desktop = self.desktop_manifest();
        let mut request = broker::DesktopRequest {
            helper_lease: self.core.lease()?.to_hex(),
            helper_operation: operation()?.to_hex(),
            broker_lease: self.hooks.receipt.as_ref().unwrap().broker_lease.clone(),
            binding_digest: self.hooks.header.digest()?.to_hex(),
            activation_nonce: nonce()?.to_hex(),
            slot: desktop.slot.clone(),
            prior_binding_digest: self.hooks.entry.as_ref().unwrap().binding_digest.clone(),
            action: broker::DesktopAction::Activate {
                owned_lifetime: self.hooks.manifest.desktop_owned_lifetime,
                transition,
                mode: self.hooks.mode,
                entry_digest: desktop.entry_digest.clone(),
                expectation_digest: desktop.expectation_digest.clone(),
                close_binding: broker::CooperativeCloseBinding {
                    operation: operation()?.to_hex(),
                    action_digest: "00".repeat(32),
                    nonce: nonce()?.to_hex(),
                },
            },
        };
        let digest = request.close_request()?.digest()?.to_hex();
        if let broker::DesktopAction::Activate { close_binding, .. } = &mut request.action {
            close_binding.action_digest = digest;
        }
        // Complete original close bytes are committed inside this exact Activate.
        self.hooks.close = Some(request.close_request()?);
        self.hooks.activation = Some(request.clone());
        if self.hooks.mode == broker::DesktopMode::OwnedExecutable {
            let explicit_owned = self.hooks.manifest.desktop_owned_lifetime.is_some();
            let evidence = self.evidence()?;
            evidence.dispositions.primary = Disposition::Unknown {
                reason: "current original primary unproved".into(),
            };
            if explicit_owned {
                ensure!(
                    matches!(
                        evidence.dispositions.descendant_scope,
                        Disposition::NotStarted
                    ),
                    "owned unavailable tree cannot clear historical uncertainty"
                );
            } else {
                evidence.dispositions.descendant_scope = Disposition::Unknown {
                    reason: "current original descendant scope unproved".into(),
                };
            }
        } else if self.hooks.requirement.lifetime == LifetimeRequirement::OwnedPrimaryExited {
            self.evidence()?.dispositions.primary = Disposition::Unknown {
                reason: "current original native primary unproved".into(),
            };
        }
        self.hooks.state = Some(self.dispatch_action(request)?);
        Ok(())
    }
    fn validate_state(&self, state: &broker::BrokerState) -> Result<()> {
        state.validate()?;
        ensure!(
            state.owned_lifetime.as_ref().map(|s| s.definition)
                == self.hooks.manifest.desktop_owned_lifetime,
            "original owned lifetime differs from committed manifest declaration"
        );
        if let Some(native) = &self.hooks.manifest.desktop_native {
            let observed = state
                .native_lifetime
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("native original lifetime projection missing"))?;
            native.validate_state(observed)?;
            if let broker::BrokerEffect::Receipt { receipt } = &state.effect {
                ensure!(
                    receipt.evidence == broker::DesktopEvidence::NativeActivation,
                    "native action borrowed owned executable evidence"
                );
            }
        } else {
            ensure!(
                state.native_lifetime.is_none(),
                "owned action acquired undeclared native authority"
            );
            if let broker::BrokerEffect::Receipt { receipt } = &state.effect {
                ensure!(
                    receipt.evidence != broker::DesktopEvidence::NativeActivation,
                    "owned action borrowed native activation evidence"
                );
            }
        }
        Ok(())
    }
    fn query(&mut self, request: &broker::DesktopRequest) -> Result<broker::BrokerState> {
        let identity = request.identity()?;
        let id = self.core.session.submit(Action::QueryDesktop {
            identity: identity.clone(),
        })?;
        let v2::Reply::DesktopState {
            identity: observed,
            state,
        } = self.core.wait_reply(id)?
        else {
            bail!("original desktop query unavailable")
        };
        ensure!(
            *observed == identity,
            "desktop query original identity changed"
        );
        self.validate_state(&state)?;
        Ok(*state)
    }
    fn readiness(&mut self, phase: IntegrationPhase, first: bool) -> Result<()> {
        let activation = self
            .hooks
            .activation
            .clone()
            .ok_or_else(|| anyhow::anyhow!("original activation missing"))?;
        let identity = activation.identity()?;
        let expected = self.readiness_manifest().clone();
        let nonce = if first {
            v2::BindingId::parse(&activation.activation_nonce)?
        } else {
            nonce()?
        };
        let expectation = desktop_readiness_expectation(&expected)?;
        let challenge = ReadinessChallenge {
            resource_binding_digest: activation.prior_binding_digest.clone(),
            action_digest: identity.request_digest.clone(),
            expectation_digest: expected.digest()?.to_hex(),
            ownership_id: self.core.resource.resource().ownership_id.clone(),
            lease: identity.broker_lease.clone(),
            slot: self.hooks.requirement.resource.clone(),
            operation: identity.helper_operation.clone(),
            nonce: nonce.to_hex(),
            deadline_ms: self.hooks.requirement.readiness.deadline_ms,
        };
        self.core
            .projector
            .publish_readiness_probe(ReadinessProbeRecord {
                challenge: challenge.clone(),
                expectation,
                expectation_digest: challenge.expectation_digest.clone(),
                observation_ids: vec![],
                completion: Disposition::Unknown {
                    reason: "fresh original readiness probe published".into(),
                },
            })?;
        let id = self.core.session.submit(Action::ProbeDesktopReadiness {
            identity: identity.clone(),
            nonce,
            deadline_ms: challenge.deadline_ms,
        })?;
        let v2::Reply::DesktopReadiness {
            identity: observed,
            observation,
        } = self.core.wait_reply(id)?
        else {
            bail!("original desktop readiness unavailable")
        };
        ensure!(
            *observed == identity
                && observation.challenge_digest == challenge.digest()?
                && observation.expectation_digest == challenge.expectation_digest,
            "desktop readiness changed original probe"
        );
        observation.validate_desktop(&identity)?;
        observation.validate_desktop_peer(&self.hooks.header.broker_principal)?;
        self.hooks.state = Some(self.query(&activation)?);
        self.hooks.sequence += 1;
        let state = self.hooks.state.clone().unwrap();
        let evidence = match &state.effect {
            broker::BrokerEffect::Receipt { receipt } => Some(receipt.evidence),
            _ => None,
        };
        let binding = self.binding(
            phase,
            activation.prior_binding_digest.clone(),
            observation.state != v2::ReadinessState::Incomplete,
            Some(activation.helper_operation.clone()),
        )?;
        let activated =
            matches!(&state.effect,broker::BrokerEffect::Receipt {receipt} if receipt.acknowledged);
        let bound = BoundDesktopObservation {
            generation: activation.transition().map(desktop_generation_evidence),
            entry_valid: None,
            binding: binding.clone(),
            broker_state: Some(state),
            readiness: Some(*observation),
            evidence,
            entry_present: IntegrationProvenance::Unavailable {
                reason: "readiness does not inspect current entry presence".into(),
            },
            activated: if activated {
                IntegrationProvenance::Observed {
                    value: true,
                    source: "original authenticated broker activation receipt".into(),
                }
            } else {
                IntegrationProvenance::Unavailable {
                    reason: "original activation effect receipt unproved".into(),
                }
            },
            primary_exited: IntegrationProvenance::Unavailable {
                reason: "readiness proves no primary exit".into(),
            },
            descendant_scope_settled: IntegrationProvenance::Unavailable {
                reason: "readiness proves no descendant join".into(),
            },
        };
        let completion = desktop_readiness_completion(&bound);
        self.core.projector.record_readiness_probe_completion(
            &challenge.ownership_id,
            &challenge.operation,
            &challenge.nonce,
            &challenge.expectation_digest,
            vec![binding.id.clone()],
            completion,
        )?;
        self.record_observation(bound)
    }
    fn close_original(&mut self, phase: IntegrationPhase) -> Result<()> {
        let Some(close) = self.hooks.close.clone() else {
            return Ok(());
        };
        let activation = self.hooks.activation.clone().unwrap();
        let published = matches!(self.core.projector.journal(),StoredJournal::NativeIntegration(j) if j.integration_actions.iter().any(|a|a.lease==close.helper_lease && a.operation==close.helper_operation));
        self.hooks.state = Some(if published {
            self.query(&close)?
        } else {
            self.dispatch_action(close.clone())?
        });
        let cancel_id = self.core.session.submit(Action::CancelDesktop {
            identity: activation.identity()?,
        })?;
        let v2::Reply::DesktopState {
            identity: cancelled,
            state,
        } = self.core.wait_reply(cancel_id)?
        else {
            bail!("original desktop cleanup unavailable")
        };
        ensure!(
            *cancelled == activation.identity()?,
            "cleanup changed original desktop identity"
        );
        self.hooks.state = Some(*state);
        let end = Instant::now()
            + Duration::from_millis(self.hooks.requirement.readiness.deadline_ms.into());
        loop {
            let id = self.core.session.submit(Action::CancelDesktop {
                identity: activation.identity()?,
            })?;
            let _ = self.core.wait_reply(id)?;
            let state = self.query(&activation)?;
            let done = settled_original_broker_state(&state);
            self.hooks.state = Some(state);
            let close_state = self.query(&close)?;
            if done && settled_original_broker_state(&close_state) {
                break;
            }
            if Instant::now() >= end {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let state = self.hooks.state.clone().unwrap();
        self.hooks.sequence += 1;
        let evidence = match &state.effect {
            broker::BrokerEffect::Receipt { receipt } => Some(receipt.evidence),
            _ => None,
        };
        let binding = self.binding(
            phase,
            activation.prior_binding_digest.clone(),
            true,
            Some(activation.helper_operation.clone()),
        )?;
        let activated =
            matches!(&state.effect,broker::BrokerEffect::Receipt {receipt} if receipt.acknowledged);
        let bound = BoundDesktopObservation {
            generation: activation.transition().map(desktop_generation_evidence),
            entry_valid: None,
            binding,
            broker_state: Some(state.clone()),
            readiness: None,
            evidence,
            entry_present: IntegrationProvenance::Unavailable {
                reason: "lifetime query does not inspect current entry presence".into(),
            },
            activated: if activated {
                IntegrationProvenance::Observed {
                    value: true,
                    source: "original broker activation receipt".into(),
                }
            } else {
                IntegrationProvenance::Unavailable {
                    reason: "original activation effect unproved".into(),
                }
            },
            primary_exited: if state
                .native_lifetime
                .as_ref()
                .is_some_and(|n| n.primary == broker::NativePrimaryState::Exited)
                || (state.native_lifetime.is_none()
                    && state.primary == broker::BrokerPrimary::Exited)
            {
                IntegrationProvenance::Observed {
                    value: true,
                    source: if state.native_lifetime.is_some() {
                        "original native child/HANDLE observed exit"
                    } else {
                        "original Host process owner observed exit"
                    }
                    .into(),
                }
            } else {
                IntegrationProvenance::Unavailable {
                    reason: "original primary exit unproved".into(),
                }
            },
            descendant_scope_settled: if state.descendants == broker::BrokerDescendants::Joined {
                IntegrationProvenance::Observed {
                    value: true,
                    source: "original Host group/job joined".into(),
                }
            } else {
                IntegrationProvenance::Unavailable {
                    reason: if state.owned_lifetime.as_ref().is_some_and(|s| s.descendants == broker::OwnedTreeState::Unavailable) {
                        "original backend has no descendant-tree capability; primary/local retirement is separate"
                    } else { "original descendant scope unproved" }.into(),
                }
            },
        };
        for previous in self.evidence()?.desktops[0]
            .observations
            .iter()
            .filter(|o| o.binding.action == bound.binding.action)
            .filter_map(|o| o.broker_state.as_ref())
        {
            validate_broker_progress(previous, &state)?;
        }
        let original = ActionReference {
            operation: activation.helper_operation.clone(),
            request_digest: v2::request_digest(&v2::Request::DesktopAction {
                request: Box::new(activation.clone()),
            })?
            .to_hex(),
        };
        let evidence = self.evidence()?;
        if let Some(owned) = &state.owned_lifetime {
            ensure!(
                activation.owned_lifetime_definition() == Some(owned.definition)
                    && evidence.desktops[0].actions.contains(&original),
                "owned lifetime changed original activation declaration/digest"
            );
            evidence.owned_lifetime = Some(OwnedDesktopLifetimeProjection {
                original,
                state: owned.clone(),
            });
            evidence.dispositions.primary = if owned.primary == broker::OwnedPrimaryState::Exited {
                Disposition::Complete {
                    receipt: "original owned primary exited".into(),
                }
            } else {
                Disposition::Unknown {
                    reason: "original owned primary exit unproved".into(),
                }
            };
            if owned.descendants == broker::OwnedTreeState::Unavailable {
                ensure!(
                    matches!(
                        evidence.dispositions.descendant_scope,
                        Disposition::NotStarted
                    ),
                    "owned unavailable tree cannot clear historical uncertainty"
                );
            } else {
                evidence.dispositions.descendant_scope = Disposition::Unknown {
                    reason: "original owned descendant uncertainty retained".into(),
                };
            }
            return self.record_observation(bound);
        }
        if let Some(native) = &state.native_lifetime {
            ensure!(
                evidence.desktops[0].actions.contains(&original),
                "native lifetime changed original activation digest"
            );
            evidence.native_lifetime = Some(NativeDesktopLifetimeProjection {
                original,
                state: native.clone(),
            });
            use broker::{NativeLifetimeAuthority, NativePrimaryState, NativeTreeState};
            evidence.dispositions.primary = if native.authority
                == NativeLifetimeAuthority::ManagerInstance
                && native.primary == NativePrimaryState::Unavailable
            {
                ensure!(
                    matches!(evidence.dispositions.primary, Disposition::NotStarted),
                    "unavailable native primary cannot clear historical uncertainty"
                );
                Disposition::NotStarted
            } else if native.primary == NativePrimaryState::Exited {
                Disposition::Complete {
                    receipt: "original native child/HANDLE exited".into(),
                }
            } else {
                Disposition::Unknown {
                    reason: "required original native primary exit unproved".into(),
                }
            };
            evidence.dispositions.descendant_scope =
                if native.descendants == NativeTreeState::Unavailable {
                    ensure!(
                        matches!(
                            evidence.dispositions.descendant_scope,
                            Disposition::NotStarted
                        ),
                        "unavailable native tree cannot clear historical uncertainty"
                    );
                    Disposition::NotStarted
                } else {
                    Disposition::Unknown {
                        reason: "historical native descendant scope unknown".into(),
                    }
                };
            return self.record_observation(bound);
        }
        evidence.dispositions.primary = if state.primary == broker::BrokerPrimary::Exited {
            Disposition::Complete {
                receipt: "original primary exited".into(),
            }
        } else {
            Disposition::Unknown {
                reason: "original primary exit unproved".into(),
            }
        };
        evidence.dispositions.descendant_scope =
            if state.descendants == broker::BrokerDescendants::Joined && state.cleanup_complete {
                Disposition::Complete {
                    receipt: "original descendant scope and IPC joined".into(),
                }
            } else {
                Disposition::Unknown {
                    reason: "original descendant scope/IPC settlement unproved".into(),
                }
            };
        self.record_observation(bound)
    }
}
impl sealed::Sealed for DesktopDriver<'_, '_, '_> {}
impl DesktopDriver<'_, '_, '_> {
    pub(crate) fn before_step_hook(&mut self, step: StepName) -> Result<()> {
        let phase = match step {
            StepName::Upgrade => Some(IntegrationPhase::BeforeUpgrade),
            StepName::Reinstall => Some(IntegrationPhase::BeforeReinstall),
            StepName::Uninstall => Some(IntegrationPhase::BeforeUninstall),
            _ => None,
        };
        if let Some(phase) = phase
            && let Err(error) = self.close_original(phase)
        {
            self.hooks.forward = false;
            self.hooks.deferred_failure = Some(match self.hooks.deferred_failure.take() {
                Some(primary) => anyhow::Error::new(DesktopCleanupFailure {
                    primary,
                    secondary: error,
                }),
                None => error,
            });
            let scenario_id = self.core.resource.resource().scenario_id.clone();
            let slot = self.hooks.requirement.resource.clone();
            let lifetime = self.hooks.requirement.lifetime;
            let definition = self
                .hooks
                .definition
                .as_ref()
                .map(|d| DefinitionKey::from_desktop_wire(d.key));
            let id = desktop_scenario_check_specs(&self.hooks.requirement, self.hooks.scenario)
                .into_iter()
                .find(|(_, p, predicate, _)| {
                    *p == phase && matches!(predicate, IntegrationPredicate::Lifetime(_))
                })
                .map(|s| s.0)
                .unwrap_or_else(|| "desktop-original-lifetime".into());
            let evidence = self.evidence()?;
            if !evidence.checks.iter().any(|c| c.id == id) {
                evidence.checks.push(IntegrationCheck {id,scenario_id,phase,slot,role:definition.as_ref().map(|d|d.role),artifact:definition.as_ref().map(|d|d.artifact),predicate:IntegrationPredicate::Lifetime(lifetime),origin:CheckOrigin::RehearsalControl,sut_operation:None,observation_ids:vec![],outcome:CheckOutcome::Incomplete,detail:"original desktop close/lifetime/local retirement unresolved; forward SUT mutation blocked and declared Uninstall retained".into()});
            }
            self.core.projector.save_report()?;
        }
        Ok(())
    }
    pub(crate) fn after_step_hook(&mut self, step: StepName) -> Result<()> {
        if step == StepName::Reinstall && self.core.forward_active() {
            self.hooks.after_reinstall = true;
        }
        if step == StepName::Exercise && self.core.forward_active() && self.hooks.forward {
            self.readiness(
                super::driver::exercise_phase(self.hooks.scenario, self.hooks.after_reinstall),
                false,
            )?;
        }
        Ok(())
    }
    pub(crate) fn checkpoint_hook(
        &mut self,
        checkpoint: Checkpoint,
        _configured: bool,
    ) -> Result<()> {
        if (!self.core.forward_active() || !self.hooks.forward)
            && checkpoint != Checkpoint::Uninstalled
        {
            return Ok(());
        }
        match checkpoint {
            Checkpoint::BaseInstalled => {
                self.inspect(IntegrationPhase::BaseInstalled)?;
                self.hooks.first_install = false;
                Ok(())
            }
            Checkpoint::TargetInstalled if self.hooks.first_install => {
                self.inspect(IntegrationPhase::TargetInstalledInitial)?;
                self.hooks.first_install = false;
                Ok(())
            }
            Checkpoint::Configured => {
                if !self.hooks.manifest.desktop_definitions.is_empty() {
                    self.inspect(IntegrationPhase::ConfiguredBeforeActivation)?;
                }
                if self.hooks.forward {
                    self.activate()?;
                    self.readiness(IntegrationPhase::Configured, true)?;
                }
                Ok(())
            }
            Checkpoint::TargetInstalled | Checkpoint::Reinstalled => {
                let phase = if checkpoint == Checkpoint::Reinstalled {
                    IntegrationPhase::Reinstalled
                } else {
                    IntegrationPhase::TargetInstalledAfterUpgrade
                };
                self.inspect(phase)?;
                if self.hooks.forward {
                    self.activate()?;
                    self.readiness(phase, true)?;
                }
                Ok(())
            }
            Checkpoint::Uninstalled => self.inspect(IntegrationPhase::Uninstalled),
            _ => bail!("unsupported ordinary desktop checkpoint"),
        }
    }
}

impl LifecycleBackend for DesktopDriver<'_, '_, '_> {
    fn step(&mut self, step: StepName, name: &str) -> Result<()> {
        if !self.hooks.forward && step != StepName::Uninstall {
            return Ok(());
        }
        self.before_step_hook(step)?;
        if !self.hooks.forward && step != StepName::Uninstall {
            return Ok(());
        }
        self.core.step(step, name)?;
        self.after_step_hook(step)
    }
    fn checkpoint(&mut self, checkpoint: Checkpoint, configured: bool) -> Result<()> {
        if !self.hooks.forward && checkpoint != Checkpoint::Uninstalled {
            return Ok(());
        }
        self.core.checkpoint(checkpoint, configured)?;
        self.checkpoint_hook(checkpoint, configured)
    }
    fn before_reinstall(&mut self) -> Result<()> {
        if self.hooks.forward && self.core.forward_active() {
            self.core.before_reinstall()?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct DesktopCleanupFailure {
    primary: anyhow::Error,
    secondary: anyhow::Error,
}
impl std::fmt::Display for DesktopCleanupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::hint::black_box(&self.secondary);
        f.write_str("original desktop cleanup and subsequent SUT cleanup both failed")
    }
}
impl std::error::Error for DesktopCleanupFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.primary.as_ref())
    }
}

#[cfg(test)]
pub(crate) mod readiness_role_review_tests {
    use super::*;
    pub(crate) fn manifest(role: &str, artifact: &str) -> v2::IntegrationManifest {
        let expectation = v2::ReadinessDefinitionExpectation {
            role: role.into(),
            artifact: artifact.into(),
            artifact_length: 123,
            artifact_sha256: "33".repeat(32),
            version_literal: "target\n".into(),
            data_sha256: "44".repeat(32),
            configuration_digest: "55".repeat(32),
        };
        let manifest:v2::IntegrationManifest=serde_json::from_value(serde_json::json!({"schema":1,"core_manifest_digest":"11".repeat(32),"desktop":{"schema":1,"core_manifest_digest":"11".repeat(32),"slot":"desktop","entry_root":"/fixture/entries","namespace_policy":"exclusive_quiescent","entry":"fixture.desktop","entry_digest":"22".repeat(32),"executable_digest":"33".repeat(32),"allowed_modes":["owned_executable"],"ready_endpoint":{"kind":"unix_path","path":"/fixture/ready.sock"},"close_endpoint":{"kind":"unix_path","path":"/fixture/close.sock"},"expectation_digest":expectation.digest().unwrap().to_hex()},"desktop_executable":{"root":"/fixture","relative_path":"app","length":123,"sha256":"33".repeat(32),"arguments":["/fixture/app"],"environment":[],"working_directory":"/fixture"},"desktop_readiness":expectation,"definitions":[],"artifact_slots":[{"slot":"desktop","relative_path":"fixture.desktop","length":128,"sha256":"22".repeat(32)}],"readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":["inspect_desktop","owned_executable","request_close"]})).unwrap();
        manifest.validate().unwrap();
        let integration = manifest.digest().unwrap();
        assert_ne!(
            v2::integration_manifest_digest(
                v2::Digest::parse(&manifest.core_manifest_digest).unwrap(),
                integration
            )
            .as_bytes(),
            &[0; 32]
        );
        manifest
    }
    #[test]
    fn desktop_readiness_preserves_complete_admitted_role_and_artifact_digest() {
        for (role, artifact) in [
            ("target", "target"),
            ("base", "base"),
            ("configured", "target"),
        ] {
            let manifest = manifest(role, artifact);
            let original = manifest.desktop_readiness.as_ref().unwrap();
            let saved = desktop_readiness_expectation(original).unwrap();
            assert_eq!(
                saved.wire(),
                *original,
                "an admitted tuple must not be relabeled after activation"
            );
            assert_eq!(
                saved.digest().unwrap(),
                manifest.desktop.as_ref().unwrap().expectation_digest
            );
        }
    }
}

#[cfg(test)]
mod native_selection_tests {
    use super::*;
    #[test]
    fn selected_native_requirement_matches_exact_platform_ceiling() {
        let definition = v2::NativeDesktopDefinition::WindowsShortcut {
            bootstrap: v2::NativeBootstrap::TheseusFixtureReadyBootstrapV1,
            lifetime: broker::NativeLifetimeRequirement::OwnedPrimaryExited,
        };
        assert_eq!(
            selected_desktop_mode(
                &[DesktopMode::NativeActivation],
                LifetimeRequirement::OwnedPrimaryExited,
                Some(&definition)
            )
            .unwrap(),
            broker::DesktopMode::NativeActivation
        );
        assert!(
            selected_desktop_mode(
                &[DesktopMode::NativeActivation],
                LifetimeRequirement::ActivationInstanceTerminated,
                Some(&definition)
            )
            .is_err()
        );
        assert!(
            selected_desktop_mode(
                &[DesktopMode::NativeActivation],
                LifetimeRequirement::OwnedProcessTreeSettled,
                Some(&definition)
            )
            .is_err()
        );
        assert!(
            selected_desktop_mode(
                &[DesktopMode::OwnedExecutable],
                LifetimeRequirement::OwnedPrimaryExited,
                Some(&definition)
            )
            .is_err()
        );
        assert!(
            selected_desktop_mode(
                &[DesktopMode::NativeActivation],
                LifetimeRequirement::OwnedPrimaryExited,
                None
            )
            .is_err()
        );
        assert!(
            selected_desktop_mode(
                &[DesktopMode::OwnedExecutable, DesktopMode::NativeActivation],
                LifetimeRequirement::OwnedPrimaryExited,
                Some(&definition)
            )
            .is_err()
        );
        assert_eq!(
            selected_desktop_mode(
                &[DesktopMode::OwnedExecutable],
                LifetimeRequirement::OwnedProcessTreeSettled,
                None
            )
            .unwrap(),
            broker::DesktopMode::OwnedExecutable
        );
    }
}

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../tests/support/native_desktop_app.rs"]
mod desktop_app_fixture;

#[cfg(all(test, target_os = "linux"))]
#[allow(dead_code)]
#[path = "../../../tests/support/native_desktop_installer.rs"]
mod desktop_installer_fixture;

#[cfg(test)]
mod combined_registration_tests {
    use super::*;
    #[test]
    fn desktop_absence_cannot_clear_independent_service_failure_or_uncertainty() {
        let complete = Disposition::Complete {
            receipt: "exact removal".into(),
        };
        for service in [
            Disposition::Unknown {
                reason: "service removal unproved".into(),
            },
            Disposition::Failed {
                reason: "service leaked registration".into(),
            },
            Disposition::NotStarted,
        ] {
            assert_eq!(
                registration_removal_disposition(Some(&service), complete.clone()),
                service
            );
        }
        let failed = Disposition::Failed {
            reason: "desktop entry leaked".into(),
        };
        assert_eq!(
            registration_removal_disposition(Some(&complete), failed.clone()),
            failed
        );
        assert_eq!(
            registration_removal_disposition(None, complete.clone()),
            complete
        );
    }
}

#[cfg(test)]
#[path = "desktop_admission_tests.rs"]
mod native_ordinary_admission_tests;

#[cfg(all(test, feature = "native-integration-test-support"))]
mod macos_owned_admission_tests {
    use super::*;
    fn owned_fixture() -> (
        ValidatedIntegrationRecipe,
        v2::IntegrationManifest,
        serde_json::Value,
    ) {
        let mut recipe = ValidatedIntegrationRecipe::decode(
            include_bytes!("../../../examples/native/service-recipe.example.json"),
            "pure-recipe.json".into(),
        )
        .unwrap();
        recipe.recipe.integrations.service = None;
        recipe.recipe.integrations.desktop = Some(DesktopRequirement {
            resource: ResourceSlot::new("desktop").unwrap(),
            modes: vec![DesktopMode::OwnedExecutable],
            readiness: ReadinessRequirement {
                kind: ReadyKind::LocalChallenge,
                protocol: ReadyProtocol::TheseusFixtureReadyV1,
                deadline_ms: 1000,
            },
            lifetime: LifetimeRequirement::OwnedPrimaryExited,
        });
        recipe.core.recipe.target.length = 123;
        recipe.core.recipe.target.sha256 = "33".repeat(32);
        let manifest = readiness_role_review_tests::manifest("target", "target");
        let raw = serde_json::json!({"profile":"native_integration_broker_v1","binding":"01".repeat(16),"manifest_digest":"11".repeat(32),"workspace_marker":"02".repeat(16),"os":"linux","target_id":"fixture","ownership_id":"03".repeat(16),"provider_epoch":"04".repeat(32),"session_id":"supplied","session_epoch":"05".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},"allowed_modes":["owned_executable"]});
        (recipe, manifest, raw)
    }
    #[test]
    fn macos_owned_primary_admission_refuses_without_changing_other_platforms() {
        let (mut recipe, manifest, raw) = owned_fixture();
        for os in ["linux", "windows", "macos"] {
            let mut raw = raw.clone();
            raw["os"] = os.into();
            if os == "macos" {
                raw["display"] = serde_json::json!({"kind":"mac_os_gui","domain":"gui/1000"});
            }
            if os == "windows" {
                raw["display"] = serde_json::json!({"kind":"source_owned_headless"});
                raw["broker_endpoint"] =
                    serde_json::json!({"kind":"named_pipe","path":r"\\.\pipe\theseus.fixture"});
                for key in ["broker_principal", "helper_principal"] {
                    raw[key] =
                        serde_json::json!({"kind":"windows","token_user_sid":"S-1-5-21-1000"});
                }
            }
            let mut header: broker::IntegrationBrokerHeader = serde_json::from_value(raw).unwrap();
            header.validate().unwrap();
            let selected = validate_admission(&recipe, &manifest, &header, ScenarioKind::Clean);
            if os != "macos" {
                assert_eq!(selected.unwrap(), broker::DesktopMode::OwnedExecutable);
                continue;
            }
            let error = selected.expect_err("macOS OwnedPrimary cannot qualify product release");
            assert!(
                error
                    .to_string()
                    .contains("OwnedExecutable lifetime on macOS")
            );
            assert!(error.to_string().contains("NativeActivation"));
            let requirement = recipe.recipe.integrations.desktop.as_mut().unwrap();
            requirement.modes = vec![DesktopMode::NativeActivation];
            requirement.lifetime = LifetimeRequirement::ActivationInstanceTerminated;
            header.allowed_modes = vec![broker::DesktopMode::NativeActivation];
            header.macos_security_session_id = Some(1);
            let mut raw = serde_json::to_value(&manifest).unwrap();
            raw["desktop_native"] = serde_json::json!({"kind":"macos_bundle","bootstrap":"theseus_fixture_ready_bootstrap_v1","lifetime":"activation_instance_terminated","app_leaf":"Fixture.app","bundle_identifier":"org.theseus.fixture","executable_leaf":"app","info_plist":{"length":128,"sha256":"22".repeat(32)}});
            raw["desktop"]["allowed_modes"] = serde_json::json!(["native_activation"]);
            raw["desktop"]["entry"] = "Fixture.app".into();
            raw["desktop_executable"]["root"] = "/fixture/entries".into();
            raw["desktop_executable"]["relative_path"] = "Fixture.app/Contents/MacOS/app".into();
            raw["desktop_executable"]["arguments"][0] =
                "/fixture/entries/Fixture.app/Contents/MacOS/app".into();
            raw["artifact_slots"][0]["relative_path"] = "Fixture.app/Contents/Info.plist".into();
            raw["desktop_capabilities"] =
                serde_json::json!(["inspect_desktop", "native_activation", "request_close"]);
            let manifest = serde_json::from_value(raw).unwrap();
            assert_eq!(
                validate_admission(&recipe, &manifest, &header, ScenarioKind::Clean).unwrap(),
                broker::DesktopMode::NativeActivation
            );
        }
    }

    #[test]
    fn explicit_owned_primary_declaration_is_required_and_platform_bound() {
        let (recipe, mut manifest, raw) = owned_fixture();
        manifest.desktop_owned_lifetime =
            Some(broker::OwnedDesktopLifetimeDefinition::MacosOriginalPrimaryV1);
        for os in ["macos", "linux"] {
            let mut raw = raw.clone();
            raw["os"] = os.into();
            if os == "macos" {
                raw["display"] = serde_json::json!({"kind":"source_owned_headless"});
            }
            let header: broker::IntegrationBrokerHeader = serde_json::from_value(raw).unwrap();
            let selected = validate_admission(&recipe, &manifest, &header, ScenarioKind::Clean);
            if os == "macos" {
                assert_eq!(selected.unwrap(), broker::DesktopMode::OwnedExecutable);
            } else {
                assert!(
                    selected.is_err(),
                    "Mac declaration cannot grant another platform authority"
                );
            }
        }
    }

    #[test]
    fn interactive_owned_windows_requires_native_entry_declaration() {
        let (recipe, manifest, mut raw) = owned_fixture();
        raw["os"] = "windows".into();
        raw["display"] = serde_json::json!({"kind":"windows_interactive","session_id":1,"window_station":"WinSta0","desktop":"Default"});
        let slash = char::from(92);
        let pipe = format!("{slash}{slash}.{slash}pipe{slash}theseus.fixture");
        raw["broker_endpoint"] = serde_json::json!({"kind":"named_pipe","path":pipe});
        for key in ["broker_principal", "helper_principal"] {
            raw[key] = serde_json::json!({"kind":"windows","token_user_sid":"S-1-5-21-1000"});
        }
        let header: broker::IntegrationBrokerHeader = serde_json::from_value(raw.clone()).unwrap();
        assert!(
            validate_admission(&recipe, &manifest, &header, ScenarioKind::Clean).is_err(),
            "pinned source bytes are not actual native shortcut qualification"
        );
        raw["display"] = serde_json::json!({"kind":"source_owned_headless"});
        let source: broker::IntegrationBrokerHeader = serde_json::from_value(raw).unwrap();
        assert_eq!(
            validate_admission(&recipe, &manifest, &source, ScenarioKind::Clean).unwrap(),
            broker::DesktopMode::OwnedExecutable
        );
    }
}
