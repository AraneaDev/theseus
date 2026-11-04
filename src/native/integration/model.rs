//! Strict schema2 evidence; serialized identities never reconstruct native owners.
use crate::model::{CheckOutcome, RunOutcome, ScenarioKind};
use crate::native::model::*;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub type Digest = String;
pub type OperationId = String;
pub type LeaseId = String;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagerKind {
    SystemdUser,
    SystemdSystem,
    LaunchdUser,
    LaunchdSystem,
    LaunchdGui,
    WindowsScm,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopKind {
    Freedesktop,
    MacosBundle,
    WindowsShortcut,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationAuthorityKind {
    LiveManagerObject,
    ExclusiveOwnedRegistration,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefinitionRole {
    Base,
    Configured,
    Target,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRole {
    Base,
    Target,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAction {
    Start,
    Stop,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagerAction {
    DaemonReload,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopMode {
    NativeActivation,
    OwnedExecutable,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedBy {
    Installer,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOrigin {
    SutPostcondition,
    RehearsalControl,
    SafetyCleanup,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServicePredicate {
    Registration,
    Configuration,
    Running,
    ApplicationReady,
    Stopped,
    RegistrationAbsent,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifetimeRequirement {
    ActivationInstanceTerminated,
    OwnedPrimaryExited,
    OwnedProcessTreeSettled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadyKind {
    ManagerOnly,
    LocalChallenge,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadyProtocol {
    TheseusFixtureReadyV1,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationPhase {
    Baseline,
    BaseInstalled,
    TargetInstalled,
    TargetInstalledInitial,
    ExerciseFinal,
    ExerciseBeforeReinstall,
    ExerciseAfterReinstall,
    Configured,
    ConfiguredBeforeActivation,
    Exercised,
    Reinstalled,
    BeforeUninstall,
    Uninstalled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationResourceKind {
    Service,
    Desktop,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingBasis {
    VacantRegistration,
    ExclusiveOwnedRegistration,
    LiveScmObject,
    AdmittedActivation,
    OriginalActivationInstance,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityRevocation {
    WrongPeer,
    WrongOrigin,
    WrongObject,
    LostExclusivity,
    ManagerRestarted,
    SessionChanged,
    UnprovedBinding,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceSlot(pub String);
impl ResourceSlot {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let s = Self(value);
        s.validate()?;
        Ok(s)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn validate(&self) -> Result<()> {
        let b = self.0.as_bytes();
        ensure!(
            !b.is_empty()
                && b.len() <= 32
                && b[0].is_ascii_lowercase()
                && b.iter()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(c)),
            "invalid resource slot"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefinitionKey {
    pub role: DefinitionRole,
    pub artifact: ArtifactRole,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RehearsalControls {
    pub configured_activation: ActivationCause,
    pub upgrade_precondition: RuntimePrecondition,
    pub reinstall_precondition: RuntimePrecondition,
    pub uninstall_precondition: RuntimePrecondition,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationCause {
    Sut,
    RehearsalStart,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePrecondition {
    Running,
    HarnessStopped,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessRequirement {
    pub kind: ReadyKind,
    pub protocol: ReadyProtocol,
    pub deadline_ms: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceCheckRequirement {
    pub id: String,
    pub scenarios: Vec<ScenarioKind>,
    pub phase: IntegrationPhase,
    #[serde(deserialize_with = "required_optional")]
    pub role: Option<DefinitionRole>,
    #[serde(deserialize_with = "required_optional")]
    pub artifact: Option<ArtifactRole>,
    pub predicate: ServicePredicate,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceRequirement {
    pub resource: ResourceSlot,
    pub managed_by: ManagedBy,
    pub controls: RehearsalControls,
    pub definitions: Vec<DefinitionKey>,
    pub checks: Vec<ServiceCheckRequirement>,
    pub readiness: ReadinessRequirement,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopRequirement {
    pub resource: ResourceSlot,
    pub modes: Vec<DesktopMode>,
    pub readiness: ReadinessRequirement,
    pub lifetime: LifetimeRequirement,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationRequirements {
    #[serde(deserialize_with = "required_optional")]
    pub service: Option<ServiceRequirement>,
    #[serde(deserialize_with = "required_optional")]
    pub desktop: Option<DesktopRequirement>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionReference {
    pub operation: OperationId,
    pub request_digest: Digest,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "predicate",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum IntegrationPredicate {
    Service(ServicePredicate),
    DesktopEntryPresent,
    DesktopEntryAbsent,
    ApplicationReady,
    Lifetime(LifetimeRequirement),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provenance", rename_all = "snake_case", deny_unknown_fields)]
pub enum IntegrationProvenance<T> {
    Observed { value: T, source: String },
    ManagerReported { value: T, manager: String },
    ApplicationReported { value: T, application: String },
    ProviderReported { value: T, provider: String },
    Unavailable { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceState {
    Absent,
    Present {
        registered: bool,
        configured: bool,
        running: bool,
        definition_digest: Digest,
        origin: String,
    },
    Unknown {
        reason: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationBinding {
    pub id: String,
    pub run_id: String,
    pub scenario_id: String,
    pub ownership_id: String,
    pub lease: LeaseId,
    pub slot: ResourceSlot,
    pub request_id: String,
    #[serde(deserialize_with = "required_optional")]
    pub action: Option<OperationId>,
    pub binding_digest: Digest,
    pub source_profile: String,
    pub source_version: String,
    pub phase: IntegrationPhase,
    #[serde(deserialize_with = "required_optional")]
    pub definition: Option<DefinitionKey>,
    pub sequence: u32,
    pub started_ms: u64,
    pub finished_ms: u64,
    pub complete: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundServiceObservation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<theseus_native_protocol::v2::ServicePlatformFacts>,
    #[serde(deserialize_with = "required_optional")]
    pub readiness: Option<theseus_native_protocol::v2::ReadinessObservation>,
    pub binding: ObservationBinding,
    pub state: IntegrationProvenance<ServiceState>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundDesktopObservation {
    pub binding: ObservationBinding,
    pub entry_present: IntegrationProvenance<bool>,
    pub activated: IntegrationProvenance<bool>,
    pub primary_exited: IntegrationProvenance<bool>,
    pub descendant_scope_settled: IntegrationProvenance<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceEvidence {
    pub slot: ResourceSlot,
    pub observations: Vec<BoundServiceObservation>,
    pub actions: Vec<ActionReference>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopEvidence {
    pub slot: ResourceSlot,
    pub observations: Vec<BoundDesktopObservation>,
    pub actions: Vec<ActionReference>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationCheck {
    pub id: String,
    pub scenario_id: String,
    pub phase: IntegrationPhase,
    pub slot: ResourceSlot,
    #[serde(deserialize_with = "required_optional")]
    pub role: Option<DefinitionRole>,
    #[serde(deserialize_with = "required_optional")]
    pub artifact: Option<ArtifactRole>,
    pub predicate: IntegrationPredicate,
    pub origin: CheckOrigin,
    #[serde(deserialize_with = "required_optional")]
    pub sut_operation: Option<OperationId>,
    pub observation_ids: Vec<String>,
    pub outcome: CheckOutcome,
    pub detail: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationDispositions {
    pub helper_worker: Disposition,
    pub registration: Disposition,
    pub manager_action: Disposition,
    pub broker: Disposition,
    pub primary: Disposition,
    pub descendant_scope: Disposition,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationScenarioEvidence {
    pub scenario_id: String,
    pub ownership_id: String,
    pub services: Vec<ServiceEvidence>,
    pub desktops: Vec<DesktopEvidence>,
    pub checks: Vec<IntegrationCheck>,
    pub dispositions: IntegrationDispositions,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogRunEvidence {
    pub scenarios: Vec<NativeScenario>,
    pub identities: Vec<Provenance<String>>,
    pub provider_claims: Vec<ProviderReceipt>,
    pub failures: Vec<String>,
    pub outcome: RunOutcome,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationReport {
    pub kind: String,
    pub schema: u32,
    pub profile: String,
    pub run_id: String,
    pub recipe_digest: Digest,
    pub core_recipe_digest: Digest,
    pub core_manifest_digest: Digest,
    pub integration_manifest_digest: Digest,
    pub manifest_digest: Digest,
    pub requirements: IntegrationRequirements,
    pub compiled_requirements_digest: Digest,
    pub selected_scenarios: Vec<ScenarioKind>,
    pub catalog: CatalogRunEvidence,
    pub integrations: Vec<IntegrationScenarioEvidence>,
    pub outcome: RunOutcome,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationResourceKey {
    pub scenario_id: String,
    pub ownership_id: String,
    pub slot: ResourceSlot,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceBindingSnapshot {
    pub digest: Digest,
    pub basis: BindingBasis,
    pub evidence_ids: Vec<String>,
    #[serde(deserialize_with = "required_optional")]
    pub definition: Option<DefinitionKey>,
    pub artifact_commitment: Digest,
    pub namespace_or_session_receipt_digest: Digest,
    #[serde(deserialize_with = "required_optional")]
    pub original_action: Option<OperationId>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationResourceRecord {
    pub key: IntegrationResourceKey,
    pub kind: IntegrationResourceKind,
    pub manifest_digest: Digest,
    pub initial_binding: ResourceBindingSnapshot,
    pub binding_history: Vec<ResourceBindingSnapshot>,
    pub current_binding_digest: Digest,
    #[serde(deserialize_with = "required_optional")]
    pub revocation: Option<AuthorityRevocation>,
    pub disposition: Disposition,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum IntegrationRequest {
    Service {
        lease: LeaseId,
        operation: OperationId,
        slot: ResourceSlot,
        prior_binding_digest: Digest,
        control: ServiceAction,
    },
    Desktop {
        lease: LeaseId,
        operation: OperationId,
        slot: ResourceSlot,
        prior_binding_digest: Digest,
        mode: DesktopMode,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum IntegrationOutcome {
    Complete { receipt: Digest },
    Unknown { reason: String },
    Failed { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerOperationIdentity {
    pub helper_lease: LeaseId,
    pub helper_operation: OperationId,
    pub broker_lease: LeaseId,
    pub request_digest: Digest,
    pub binding_digest: Digest,
    pub activation_nonce: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationActionRecord {
    pub ownership_id: String,
    pub lease: LeaseId,
    pub slot: ResourceSlot,
    pub operation: OperationId,
    pub request: IntegrationRequest,
    pub request_digest: Digest,
    pub prior_binding_digest: Digest,
    #[serde(deserialize_with = "required_optional")]
    pub broker_identity: Option<BrokerOperationIdentity>,
    pub completion: Disposition,
    #[serde(deserialize_with = "required_optional")]
    pub terminal: Option<IntegrationOutcome>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerBindingRecord {
    pub ownership_id: String,
    pub helper_lease: LeaseId,
    pub slot: ResourceSlot,
    pub original_intent: NativeIntent,
    pub publication_scope: String,
    pub original_intent_reference: String,
    pub expected_header_digest: Digest,
    pub session_id: String,
    pub session_epoch: Digest,
    #[serde(deserialize_with = "required_optional")]
    pub broker_lease: Option<LeaseId>,
    pub disposition: Disposition,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessChallenge {
    pub resource_binding_digest: String,
    pub action_digest: String,
    pub expectation_digest: String,
    pub ownership_id: String,
    pub lease: LeaseId,
    pub slot: ResourceSlot,
    pub operation: OperationId,
    pub nonce: String,
    pub deadline_ms: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessExpectation {
    pub role: DefinitionRole,
    pub artifact: ArtifactRole,
    pub artifact_length: u64,
    pub configuration_digest: String,
    pub artifact_digest: Digest,
    pub version: String,
    pub data_digest: Digest,
    pub initialized: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessProbeRecord {
    pub challenge: ReadinessChallenge,
    pub expectation: ReadinessExpectation,
    pub expectation_digest: Digest,
    pub observation_ids: Vec<String>,
    pub completion: Disposition,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalQueryOutcome {
    pub original: ActionReference,
    pub outcome: IntegrationOutcome,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationRecoveryRecord {
    pub recovery_id: String,
    pub ownership_id: String,
    pub original_actions: Vec<ActionReference>,
    pub prior_local: Vec<Disposition>,
    pub local: Disposition,
    pub broker: Disposition,
    pub provider: Disposition,
    pub queries: Vec<OriginalQueryOutcome>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationJournal {
    pub kind: String,
    pub schema: u32,
    pub profile: String,
    pub run_id: String,
    pub recipe_digest: Digest,
    pub core_recipe_digest: Digest,
    pub core_manifest_digest: Digest,
    pub integration_manifest_digest: Digest,
    pub manifest_digest: Digest,
    pub requirements: IntegrationRequirements,
    pub compiled_requirements_digest: Digest,
    pub selected_scenarios: Vec<ScenarioKind>,
    #[serde(deserialize_with = "required_optional")]
    pub launch_fingerprint: Option<Digest>,
    pub ownership: Vec<NativeResource>,
    pub operations: Vec<NativeOperation>,
    pub provider_calls: Vec<NativeProviderCall>,
    pub releases: Vec<NativeRelease>,
    pub recovery_attempts: Vec<NativeRecoveryAttempt>,
    pub failures: Vec<String>,
    pub integration_ownership: Vec<IntegrationResourceRecord>,
    pub integration_actions: Vec<IntegrationActionRecord>,
    pub broker_bindings: Vec<BrokerBindingRecord>,
    pub readiness_probes: Vec<ReadinessProbeRecord>,
    pub integration_recoveries: Vec<IntegrationRecoveryRecord>,
}

#[derive(Debug, Clone)]
// Preserve the concrete versioned DTO contract; no live native owners are stored.
#[allow(clippy::large_enum_variant)]
pub enum NativeReportVersion {
    CatalogV1(NativeReport),
    IntegrationV2(NativeIntegrationReport),
}
#[derive(Debug, Clone)]
// Preserve the concrete versioned DTO contract; no live native owners are stored.
#[allow(clippy::large_enum_variant)]
pub enum NativeJournalVersion {
    CatalogV1(NativeJournal),
    IntegrationV2(NativeIntegrationJournal),
}

impl IntegrationDispositions {
    pub fn all(&self) -> [&Disposition; 6] {
        [
            &self.helper_worker,
            &self.registration,
            &self.manager_action,
            &self.broker,
            &self.primary,
            &self.descendant_scope,
        ]
    }
    pub fn complete(&self) -> bool {
        self.all()
            .iter()
            .all(|d| matches!(d, Disposition::Complete { .. }))
    }
}
impl ObservationBinding {
    pub fn validate(&self) -> Result<()> {
        for value in [
            &self.id,
            &self.run_id,
            &self.scenario_id,
            &self.ownership_id,
            &self.request_id,
            &self.source_profile,
            &self.source_version,
        ] {
            text(value, 256)?;
        }
        hex(&self.lease, 32)?;
        self.slot.validate()?;
        digest(&self.binding_digest)?;
        if let Some(action) = &self.action {
            hex(action, 32)?;
        }
        ensure!(
            self.finished_ms >= self.started_ms,
            "reversed observation interval"
        );
        Ok(())
    }
}
fn commitments(core: &str, integration: &str, full: &str) -> Result<()> {
    use theseus_native_protocol::v2;
    ensure!(
        v2::integration_manifest_digest(v2::Digest::parse(core)?, v2::Digest::parse(integration)?)
            .to_hex()
            == full,
        "integration manifest composition mismatch"
    );
    Ok(())
}
fn common_validate(
    envelope: (&str, u32),
    expected: &str,
    profile: &str,
    run: &str,
    digests: [&str; 6],
    requirements: &IntegrationRequirements,
    selected: &[ScenarioKind],
) -> Result<()> {
    let (kind, schema) = envelope;
    ensure!(
        kind == expected && schema == 2 && profile == super::PROFILE,
        "unsupported integration envelope"
    );
    text(run, 128)?;
    for d in digests {
        digest(d)?;
    }
    commitments(digests[2], digests[3], digests[4])?;
    ensure!(
        requirements.digest()? == digests[5],
        "compiled integration requirements changed"
    );
    ensure!(
        !selected.is_empty() && selected.len() <= 3,
        "invalid integration selection"
    );
    let mut seen = BTreeSet::new();
    for scenario in selected {
        ordinary(*scenario)?;
        ensure!(seen.insert(scenario), "duplicate integration selection");
    }
    requirements.validate(selected)
}
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
                    ensure!(
                        check.phase == required.phase
                            && check.slot == service.resource
                            && check.role == required.role
                            && check.artifact == required.artifact
                            && check.predicate == IntegrationPredicate::Service(required.predicate)
                            && check.origin == CheckOrigin::SutPostcondition,
                        "required integration check changed"
                    );
                }
            }
            if self.outcome == RunOutcome::Passed {
                ensure!(
                    core.outcome == RunOutcome::Passed
                        && integration
                            .checks
                            .iter()
                            .all(|c| c.outcome == CheckOutcome::Passed)
                        && integration.dispositions.complete(),
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
            lease_for(&action.ownership_id, &action.lease)?;
            hex(&action.operation, 32)?;
            digest(&action.request_digest)?;
            digest(&action.prior_binding_digest)?;
            action.slot.validate()?;
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
            lease_for(&challenge.ownership_id, &challenge.lease)?;
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
            ensure!(
                self.integration_actions
                    .iter()
                    .any(|a| a.ownership_id == challenge.ownership_id
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
                        && matches!(a.completion, Disposition::Complete { .. })),
                "readiness original Start completion missing"
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
        // Broker/session effects are capability-refused in this initial service profile.
        ensure!(
            self.broker_bindings.is_empty(),
            "broker bindings require explicit desktop capability"
        );
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

impl NativeIntegrationReport {
    pub fn new(
        run_id: String,
        recipe: &super::ValidatedIntegrationRecipe,
        selected_scenarios: Vec<ScenarioKind>,
    ) -> Result<Self> {
        Ok(Self {
            kind: "theseus_native_report".into(),
            schema: 2,
            profile: super::PROFILE.into(),
            run_id,
            recipe_digest: recipe.digest.clone(),
            core_recipe_digest: recipe.core.digest.clone(),
            core_manifest_digest: recipe.core.recipe.manifest_digest.clone(),
            integration_manifest_digest: recipe.recipe.integration_manifest_digest.clone(),
            manifest_digest: recipe.full_manifest_digest()?,
            requirements: recipe.recipe.integrations.clone(),
            compiled_requirements_digest: recipe.recipe.integrations.digest()?,
            selected_scenarios,
            catalog: CatalogRunEvidence {
                scenarios: vec![],
                identities: vec![],
                provider_claims: vec![],
                failures: vec![],
                outcome: RunOutcome::Incomplete,
            },
            integrations: vec![],
            outcome: RunOutcome::Incomplete,
        })
    }
}
impl NativeIntegrationJournal {
    pub fn new(
        run_id: String,
        recipe: &super::ValidatedIntegrationRecipe,
        selected_scenarios: Vec<ScenarioKind>,
    ) -> Result<Self> {
        Ok(Self {
            kind: "theseus_native_journal".into(),
            schema: 2,
            profile: super::PROFILE.into(),
            run_id,
            launch_fingerprint: None,
            recipe_digest: recipe.digest.clone(),
            core_recipe_digest: recipe.core.digest.clone(),
            core_manifest_digest: recipe.core.recipe.manifest_digest.clone(),
            integration_manifest_digest: recipe.recipe.integration_manifest_digest.clone(),
            manifest_digest: recipe.full_manifest_digest()?,
            requirements: recipe.recipe.integrations.clone(),
            compiled_requirements_digest: recipe.recipe.integrations.digest()?,
            selected_scenarios,
            ownership: vec![],
            operations: vec![],
            provider_calls: vec![],
            releases: vec![],
            recovery_attempts: vec![],
            failures: vec![],
            integration_ownership: vec![],
            integration_actions: vec![],
            broker_bindings: vec![],
            readiness_probes: vec![],
            integration_recoveries: vec![],
        })
    }
}

impl ReadinessExpectation {
    pub fn wire(&self) -> theseus_native_protocol::v2::ReadinessDefinitionExpectation {
        theseus_native_protocol::v2::ReadinessDefinitionExpectation {
            role: match self.role {
                DefinitionRole::Base => "base",
                DefinitionRole::Configured => "configured",
                DefinitionRole::Target => "target",
            }
            .into(),
            artifact: match self.artifact {
                ArtifactRole::Base => "base",
                ArtifactRole::Target => "target",
            }
            .into(),
            artifact_length: self.artifact_length,
            artifact_sha256: self.artifact_digest.clone(),
            version_literal: self.version.clone(),
            data_sha256: self.data_digest.clone(),
            configuration_digest: self.configuration_digest.clone(),
        }
    }
    pub fn digest(&self) -> Result<String> {
        ensure!(
            self.initialized,
            "readiness expectation requires initialized application"
        );
        Ok(self.wire().digest()?.to_hex())
    }
}
impl ReadinessChallenge {
    pub fn digest(&self) -> Result<String> {
        Ok(theseus_native_protocol::v2::ReadinessChallengeFields {
            lease: self.lease.clone(),
            slot: self.slot.as_str().into(),
            resource_binding_digest: self.resource_binding_digest.clone(),
            operation: self.operation.clone(),
            action_digest: self.action_digest.clone(),
            nonce: self.nonce.clone(),
            expectation_digest: self.expectation_digest.clone(),
        }
        .digest()?
        .to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn journal() -> NativeIntegrationJournal {
        let report = crate::native::integration::view::tests::report();
        NativeIntegrationJournal {
            kind: "theseus_native_journal".into(),
            schema: 2,
            profile: super::super::PROFILE.into(),
            run_id: report.run_id,
            launch_fingerprint: Some("aa".repeat(32)),
            recipe_digest: report.recipe_digest,
            core_recipe_digest: report.core_recipe_digest,
            core_manifest_digest: report.core_manifest_digest,
            integration_manifest_digest: report.integration_manifest_digest,
            manifest_digest: report.manifest_digest,
            requirements: report.requirements,
            compiled_requirements_digest: report.compiled_requirements_digest,
            selected_scenarios: report.selected_scenarios,
            ownership: vec![],
            operations: vec![],
            provider_calls: vec![],
            releases: vec![],
            recovery_attempts: vec![],
            failures: vec![],
            integration_ownership: vec![],
            integration_actions: vec![],
            broker_bindings: vec![],
            readiness_probes: vec![],
            integration_recoveries: vec![],
        }
    }
    #[test]
    fn malformed_readiness_probe_cannot_enter_saved_evidence() {
        let mut journal = journal();
        journal.readiness_probes.push(ReadinessProbeRecord {
            challenge: ReadinessChallenge {
                ownership_id: "missing-owner".into(),
                lease: "11".repeat(16),
                slot: ResourceSlot::new("fixture_service").unwrap(),
                operation: "22".repeat(16),
                nonce: "33".repeat(16),
                deadline_ms: 30_001,
                resource_binding_digest: "44".repeat(32),
                action_digest: "55".repeat(32),
                expectation_digest: "66".repeat(32),
            },
            expectation: ReadinessExpectation {
                role: DefinitionRole::Configured,
                artifact: ArtifactRole::Target,
                artifact_length: 1,
                configuration_digest: "77".repeat(32),
                artifact_digest: "88".repeat(32),
                version: "target".into(),
                data_digest: "99".repeat(32),
                initialized: true,
            },
            expectation_digest: "00".repeat(32),
            observation_ids: vec![],
            completion: Disposition::Complete {
                receipt: "unproved".into(),
            },
        });
        assert!(
            journal.validate().is_err(),
            "malformed orphan probe must refuse saved evidence"
        );
    }
    #[test]
    fn schema2_missing_disposition_and_legacy_decoder_refuse() {
        let report = crate::native::integration::view::tests::report();
        let bytes = report.encode().unwrap();
        assert!(NativeReport::decode(&bytes).is_err());
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["integrations"][0]["dispositions"]
            .as_object_mut()
            .unwrap()
            .remove("helper_worker");
        assert!(NativeIntegrationReport::decode(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}

impl IntegrationRequest {
    pub fn wire(&self) -> Result<theseus_native_protocol::v2::Request> {
        use theseus_native_protocol::v2;
        match self {
            Self::Service {
                lease,
                operation,
                slot,
                prior_binding_digest,
                control,
            } => Ok(v2::Request::ServiceAction {
                lease: v2::LeaseId::parse(lease)?,
                operation: v2::OperationId::parse(operation)?,
                slot: v2::ResourceSlot::new(slot.as_str())?,
                prior_binding_digest: v2::Digest::parse(prior_binding_digest)?,
                action: match control {
                    ServiceAction::Start => v2::ServiceAction::Start,
                    ServiceAction::Stop => v2::ServiceAction::Stop,
                },
            }),
            Self::Desktop { .. } => {
                anyhow::bail!("desktop action requires explicit broker protocol capability")
            }
        }
    }
}

fn required_optional<'de, D, T>(decoder: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(decoder)
}

#[cfg(test)]
mod catalog_review_tests {
    use super::*;
    #[test]
    fn review_rejects_malformed_catalog_evidence() {
        let mut report = crate::native::integration::view::tests::report();
        report.catalog.scenarios[0].stages.push(NativeStage {
            name: "malformed-catalog".into(),
            step: None,
            checkpoint: None,
            outcome: crate::model::StageOutcome::Passed,
            commands: vec![NativeCommandEvidence {
                command: CatalogCommand {
                    id: "probe".into(),
                    program: CatalogProgram::Exercise,
                    expected_status: NativeStatus::PosixExit { code: 0 },
                    stdout_literal: None,
                },
                operation_id: "invalid".into(),
                request_digest: "not-a-digest".into(),
                status: Some(NativeStatus::PosixExit { code: 0 }),
                stdout: String::new(),
                stdout_complete: true,
                stdout_utf8: true,
                stderr_complete: true,
                stdout_log: None,
                stderr_log: None,
                failure: None,
            }],
            checks: vec![],
            native_files: vec![],
            failures: vec![],
        });
        assert!(
            report.validate().is_err(),
            "malformed catalog command must refuse schema2 evidence"
        );
        report.catalog.scenarios[0].stages[0].commands.clear();
        report.catalog.scenarios[0].stages[0]
            .native_files
            .push(NativeFileObservation {
                request_id: "inspect".into(),
                root_identity: "root".into(),
                leaf: "unexpected".into(),
                state: NativeFileState::Present {
                    length: 1,
                    sha256: "bad".into(),
                },
            });
        assert!(
            report.validate().is_err(),
            "malformed catalog file evidence must refuse"
        );
        report.catalog.scenarios[0].stages[0].native_files.clear();
        report.catalog.scenarios[0].outcome = RunOutcome::Passed;
        assert!(
            report.validate().is_err(),
            "passed catalog with unresolved core dispositions must refuse"
        );
    }
}
