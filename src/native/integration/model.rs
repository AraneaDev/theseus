//! Strict schema2 evidence; serialized identities never reconstruct native owners.
use crate::model::{CheckOutcome, RunOutcome, ScenarioKind};
use crate::native::model::*;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
    TargetInstalledAfterUpgrade,
    TargetInstalledInitial,
    ExerciseFinal,
    ExerciseBeforeReinstall,
    ExerciseAfterReinstall,
    Configured,
    ConfiguredBeforeActivation,
    Exercised,
    Reinstalled,
    BeforeUpgrade,
    BeforeReinstall,
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
pub fn desktop_check_specs(
    requirement: &DesktopRequirement,
) -> Vec<(String, IntegrationPhase, IntegrationPredicate, CheckOrigin)> {
    vec![
        (
            "desktop-baseline-absent".into(),
            IntegrationPhase::Baseline,
            IntegrationPredicate::DesktopEntryAbsent,
            CheckOrigin::SutPostcondition,
        ),
        (
            "desktop-installed-entry".into(),
            IntegrationPhase::TargetInstalledInitial,
            IntegrationPredicate::DesktopEntryPresent,
            CheckOrigin::SutPostcondition,
        ),
        (
            "desktop-configured-ready".into(),
            IntegrationPhase::Configured,
            IntegrationPredicate::ApplicationReady,
            CheckOrigin::RehearsalControl,
        ),
        (
            "desktop-exercise-ready".into(),
            IntegrationPhase::ExerciseFinal,
            IntegrationPredicate::ApplicationReady,
            CheckOrigin::RehearsalControl,
        ),
        (
            "desktop-original-lifetime".into(),
            IntegrationPhase::BeforeUninstall,
            IntegrationPredicate::Lifetime(requirement.lifetime),
            CheckOrigin::RehearsalControl,
        ),
        (
            "desktop-uninstalled-entry".into(),
            IntegrationPhase::Uninstalled,
            IntegrationPredicate::DesktopEntryAbsent,
            CheckOrigin::SutPostcondition,
        ),
    ]
}
pub fn desktop_phase_definition(
    scenario: ScenarioKind,
    phase: IntegrationPhase,
) -> Option<DefinitionKey> {
    let (role, artifact) = match phase {
        IntegrationPhase::Baseline | IntegrationPhase::Uninstalled => return None,
        IntegrationPhase::BaseInstalled => (DefinitionRole::Base, ArtifactRole::Base),
        IntegrationPhase::TargetInstalledInitial => (DefinitionRole::Target, ArtifactRole::Target),
        IntegrationPhase::Configured
        | IntegrationPhase::ConfiguredBeforeActivation
        | IntegrationPhase::BeforeUpgrade
            if scenario == ScenarioKind::Upgrade =>
        {
            (DefinitionRole::Configured, ArtifactRole::Base)
        }
        _ => (DefinitionRole::Configured, ArtifactRole::Target),
    };
    Some(DefinitionKey { role, artifact })
}
pub fn desktop_scenario_check_specs(
    requirement: &DesktopRequirement,
    scenario: ScenarioKind,
) -> Vec<(String, IntegrationPhase, IntegrationPredicate, CheckOrigin)> {
    if scenario == ScenarioKind::Clean {
        return desktop_check_specs(requirement);
    }
    use IntegrationPhase as P;
    use IntegrationPredicate as Predicate;
    let mut specs = vec![(
        "desktop-baseline-absent".into(),
        P::Baseline,
        Predicate::DesktopEntryAbsent,
        CheckOrigin::SutPostcondition,
    )];
    let initial = if scenario == ScenarioKind::Upgrade {
        P::BaseInstalled
    } else {
        P::TargetInstalledInitial
    };
    specs.push((
        "desktop-installed-entry".into(),
        initial,
        Predicate::DesktopEntryPresent,
        CheckOrigin::SutPostcondition,
    ));
    specs.push((
        "desktop-configured-entry".into(),
        P::ConfiguredBeforeActivation,
        Predicate::DesktopEntryPresent,
        CheckOrigin::SutPostcondition,
    ));
    specs.push((
        "desktop-configured-ready".into(),
        P::Configured,
        Predicate::ApplicationReady,
        CheckOrigin::RehearsalControl,
    ));
    let (before, after, label) = if scenario == ScenarioKind::Upgrade {
        (P::BeforeUpgrade, P::TargetInstalledAfterUpgrade, "upgrade")
    } else {
        (P::BeforeReinstall, P::Reinstalled, "reinstall")
    };
    if scenario == ScenarioKind::Reinstall {
        specs.push((
            "desktop-exercise-before-reinstall-ready".into(),
            P::ExerciseBeforeReinstall,
            Predicate::ApplicationReady,
            CheckOrigin::RehearsalControl,
        ));
    }
    specs.push((
        format!("desktop-before-{label}-lifetime"),
        before,
        Predicate::Lifetime(requirement.lifetime),
        CheckOrigin::RehearsalControl,
    ));
    specs.push((
        format!("desktop-after-{label}-entry"),
        after,
        Predicate::DesktopEntryPresent,
        CheckOrigin::SutPostcondition,
    ));
    specs.push((
        format!("desktop-after-{label}-ready"),
        after,
        Predicate::ApplicationReady,
        CheckOrigin::RehearsalControl,
    ));
    specs.push((
        "desktop-exercise-ready".into(),
        if scenario == ScenarioKind::Reinstall {
            P::ExerciseAfterReinstall
        } else {
            P::ExerciseFinal
        },
        Predicate::ApplicationReady,
        CheckOrigin::RehearsalControl,
    ));
    specs.push((
        "desktop-original-lifetime".into(),
        P::BeforeUninstall,
        Predicate::Lifetime(requirement.lifetime),
        CheckOrigin::RehearsalControl,
    ));
    specs.push((
        "desktop-uninstalled-entry".into(),
        P::Uninstalled,
        Predicate::DesktopEntryAbsent,
        CheckOrigin::SutPostcondition,
    ));
    specs
}
impl DefinitionKey {
    pub fn desktop_wire(&self) -> theseus_native_protocol::v2::DesktopDefinitionKey {
        use theseus_native_protocol::v2::{
            DesktopArtifactRole as A, DesktopDefinitionKey, DesktopDefinitionRole as R,
        };
        DesktopDefinitionKey {
            role: match self.role {
                DefinitionRole::Base => R::Base,
                DefinitionRole::Configured => R::Configured,
                DefinitionRole::Target => R::Target,
            },
            artifact: match self.artifact {
                ArtifactRole::Base => A::Base,
                ArtifactRole::Target => A::Target,
            },
        }
    }
    pub fn from_desktop_wire(key: theseus_native_protocol::v2::DesktopDefinitionKey) -> Self {
        use theseus_native_protocol::v2::{DesktopArtifactRole as A, DesktopDefinitionRole as R};
        Self {
            role: match key.role {
                R::Base => DefinitionRole::Base,
                R::Configured => DefinitionRole::Configured,
                R::Target => DefinitionRole::Target,
            },
            artifact: match key.artifact {
                A::Base => ArtifactRole::Base,
                A::Target => ArtifactRole::Target,
            },
        }
    }
}
pub fn validate_desktop_sut_generation(
    ownership: &str,
    lease: &str,
    witness: &theseus_native_protocol::v2::broker::DesktopSutRunWitness,
    operation: &NativeOperation,
    measured: &NativeCommandEvidence,
) -> Result<()> {
    witness.validate()?;
    operation.validate()?;
    measured.validate()?;
    ensure!(
        operation.ownership_id == ownership
            && operation.lease == lease
            && witness.lease == lease
            && operation.operation_id == witness.operation
            && operation.request_digest == witness.request_digest
            && operation.command.program.program_id() == witness.program
            && matches!(operation.completion, Disposition::Complete { .. })
            && measured.operation_id == operation.operation_id
            && measured.request_digest == operation.request_digest
            && measured.command == operation.command
            && measured.failure.is_none()
            && matches!(
                measured.status,
                Some(NativeStatus::PosixExit { code: 0 } | NativeStatus::WindowsExit { code: 0 })
            ),
        "desktop generation lacks exact successful original SUT Run and measured status"
    );
    Ok(())
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopPreviousActivationEvidence {
    pub operation: OperationId,
    /// Canonical broker request digest; exact full identity stays private.
    pub broker_request_digest: Digest,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopGenerationEvidence {
    pub definition: theseus_native_protocol::v2::DesktopDefinitionKey,
    pub tuple_digest: Digest,
    pub sut: theseus_native_protocol::v2::broker::DesktopSutRunWitness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_activation: Option<DesktopPreviousActivationEvidence>,
}
impl DesktopGenerationEvidence {
    pub fn validate(&self) -> Result<()> {
        self.sut.validate()?;
        digest(&self.tuple_digest)?;
        if let Some(previous) = &self.previous_activation {
            hex(&previous.operation, 32)?;
            digest(&previous.broker_request_digest)?;
            ensure!(
                previous.operation != self.sut.operation,
                "generation reused original activation as SUT Run"
            );
        }
        Ok(())
    }
}
/// Public evidence carries correlations, never an original private activation nonce.
pub fn desktop_generation_evidence(
    generation: &theseus_native_protocol::v2::broker::DesktopActivationTransition,
) -> DesktopGenerationEvidence {
    DesktopGenerationEvidence {
        definition: generation.definition,
        tuple_digest: generation.tuple_digest.clone(),
        sut: generation.sut.clone(),
        previous_activation: generation.previous_activation.as_ref().map(|original| {
            DesktopPreviousActivationEvidence {
                operation: original.helper_operation.clone(),
                broker_request_digest: original.request_digest.clone(),
            }
        }),
    }
}
pub(crate) fn validate_desktop_observation_generation(
    report: &NativeIntegrationReport,
    journal: &NativeIntegrationJournal,
    observation: &BoundDesktopObservation,
) -> Result<()> {
    let b = &observation.binding;
    // A legacy-looking public binding cannot erase a keyed private Activate.
    if observation.broker_state.is_some() || observation.readiness.is_some() {
        let original = journal
            .integration_actions
            .iter()
            .find(|a| {
                a.ownership_id == b.ownership_id
                    && a.lease == b.lease
                    && a.slot == b.slot
                    && b.action.as_ref() == Some(&a.operation)
            })
            .ok_or_else(|| anyhow::anyhow!("desktop generation activation missing"))?;
        ensure!(
            matches!(&original.request,IntegrationRequest::Desktop {request,..} if matches!(request.action,theseus_native_protocol::v2::broker::DesktopAction::Activate {..}) && request.transition().map(desktop_generation_evidence)==observation.generation),
            "desktop generation differs from exact accepted original Activate"
        );
    }
    let Some(generation) = &observation.generation else {
        return Ok(());
    };
    let operation = journal
        .operations
        .iter()
        .find(|op| {
            op.ownership_id == b.ownership_id
                && op.lease == b.lease
                && op.operation_id == generation.sut.operation
        })
        .ok_or_else(|| anyhow::anyhow!("desktop generation original SUT operation missing"))?;
    let measured = report
        .catalog
        .scenarios
        .iter()
        .filter(|s| s.ownership_id == b.ownership_id && s.id == b.scenario_id)
        .flat_map(|s| &s.stages)
        .flat_map(|s| &s.commands)
        .find(|c| {
            c.operation_id == generation.sut.operation
                && c.request_digest == generation.sut.request_digest
        })
        .ok_or_else(|| {
            anyhow::anyhow!("desktop generation measured original SUT command missing")
        })?;
    validate_desktop_sut_generation(
        &b.ownership_id,
        &b.lease,
        &generation.sut,
        operation,
        measured,
    )?;
    ensure!(
        b.definition == Some(DefinitionKey::from_desktop_wire(generation.definition)),
        "desktop generation definition binding changed"
    );
    if observation.broker_state.is_none() && observation.readiness.is_none() {
        ensure!(
            b.action.as_ref() == Some(&generation.sut.operation),
            "desktop entry inspection borrowed another SUT generation"
        );
    }
    if let Some(previous) = &generation.previous_activation {
        let original=journal.integration_actions.iter().find(|a|a.ownership_id==b.ownership_id && a.lease==b.lease && a.slot==b.slot && matches!(&a.request,IntegrationRequest::Desktop {request,..} if request.identity().is_ok_and(|identity|identity.helper_operation==previous.operation && identity.request_digest==previous.broker_request_digest && identity.helper_lease==b.lease))).ok_or_else(||anyhow::anyhow!("desktop generation prior original activation missing"))?;
        let prior = report
            .integrations
            .iter()
            .filter(|s| s.ownership_id == b.ownership_id)
            .flat_map(|s| &s.desktops)
            .filter(|d| d.slot == b.slot)
            .flat_map(|d| &d.observations)
            .filter(|o| o.binding.action.as_ref() == Some(&original.operation))
            .filter_map(|o| o.broker_state.as_ref())
            .next_back();
        ensure!(
            prior.is_some_and(settled_original_broker_state),
            "desktop generation replaced an unsettled/lost original activation"
        );
    }
    Ok(())
}
fn observed_bool(value: &IntegrationProvenance<bool>) -> Option<bool> {
    match value {
        IntegrationProvenance::Observed { value, .. } => Some(*value),
        _ => None,
    }
}
impl LifetimeRequirement {
    pub fn native(self) -> theseus_native_protocol::v2::broker::NativeLifetimeRequirement {
        use theseus_native_protocol::v2::broker::NativeLifetimeRequirement as Native;
        match self {
            Self::ActivationInstanceTerminated => Native::ActivationInstanceTerminated,
            Self::OwnedPrimaryExited => Native::OwnedPrimaryExited,
            Self::OwnedProcessTreeSettled => Native::OwnedProcessTreeSettled,
        }
    }
}
pub fn desktop_predicate_satisfied(
    observation: &BoundDesktopObservation,
    predicate: &IntegrationPredicate,
) -> bool {
    use theseus_native_protocol::v2::{ReadinessState, broker::*};
    let state = observation
        .broker_state
        .as_ref()
        .filter(|s| s.validate().is_ok());
    let owned = state.is_some_and(|s| matches!(&s.effect, BrokerEffect::Receipt { receipt } if receipt.acknowledged && matches!(receipt.evidence,DesktopEvidence::OwnedExecutable|DesktopEvidence::SourceOwnedExecutable)) && s.native_lifetime.is_none());
    let native = state.filter(|s| matches!(&s.effect,BrokerEffect::Receipt {receipt} if receipt.acknowledged && receipt.evidence==DesktopEvidence::NativeActivation)).and_then(|s|s.native_lifetime.as_ref());
    match predicate {
        IntegrationPredicate::DesktopEntryPresent => {
            observed_bool(&observation.entry_present) == Some(true)
                && observation.entry_valid == Some(true)
        }
        IntegrationPredicate::DesktopEntryAbsent => {
            observed_bool(&observation.entry_present) == Some(false)
        }
        IntegrationPredicate::ApplicationReady => {
            observation.readiness.as_ref().is_some_and(|r| {
                r.state == ReadinessState::Ready
                    && r.authenticated_instance
                    && r.initialized == Some(true)
                    && r.response_digest.is_some()
                    && r.resource_binding_digest == observation.binding.binding_digest
            }) && (owned || native.is_some())
        }
        IntegrationPredicate::Lifetime(required) => {
            if let Some(native) = native {
                native.required == required.native() && native.required_lifetime_satisfied()
            } else if owned {
                state.is_some_and(|s| match required {
                    LifetimeRequirement::OwnedPrimaryExited => s.primary == BrokerPrimary::Exited,
                    LifetimeRequirement::OwnedProcessTreeSettled => {
                        s.primary == BrokerPrimary::Exited
                            && s.descendants == BrokerDescendants::Joined
                            && s.cleanup_complete
                    }
                    LifetimeRequirement::ActivationInstanceTerminated => false,
                })
            } else {
                false
            }
        }
        _ => false,
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<DesktopGenerationEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_valid: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker_state: Option<theseus_native_protocol::v2::broker::BrokerState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<theseus_native_protocol::v2::ReadinessObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<theseus_native_protocol::v2::broker::DesktopEvidence>,
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
pub struct NativeDesktopLifetimeProjection {
    pub original: ActionReference,
    pub state: theseus_native_protocol::v2::broker::NativeLifetimeState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedDesktopLifetimeProjection {
    pub original: ActionReference,
    pub state: theseus_native_protocol::v2::broker::OwnedLifetimeState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationScenarioEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_lifetime: Option<OwnedDesktopLifetimeProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_lifetime: Option<NativeDesktopLifetimeProjection>,
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
        request: Box<theseus_native_protocol::v2::broker::DesktopRequest>,
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
pub struct BrokerIntentRecord {
    pub intent: String,
    pub binding: String,
    pub manifest_digest: String,
    pub workspace_marker: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerBindingRecord {
    pub original: theseus_native_protocol::v2::broker::BrokerBind,
    pub ownership_id: String,
    pub helper_lease: LeaseId,
    pub slot: ResourceSlot,
    pub original_intent: BrokerIntentRecord,
    pub publication_scope: Option<theseus_native_protocol::v2::broker::IntentPublicationScope>,
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
    pub view: OriginalQueryView,
    pub outcome: IntegrationOutcome,
}
/// Authenticated original broker lifetime knowledge, separate from the immutable
/// application/activation effect terminal stored in OriginalQueryOutcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerObservationSource {
    Query,
    Cancel,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveredBrokerObservation {
    pub original: ActionReference,
    pub identity: theseus_native_protocol::v2::broker::BrokerOperationIdentity,
    pub source: BrokerObservationSource,
    pub state: theseus_native_protocol::v2::broker::BrokerState,
}
/// Immutable knowledge at recovery publication, before any Query or Cancel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalBrokerEffectBaseline {
    pub original: ActionReference,
    #[serde(deserialize_with = "required_optional")]
    pub terminal: Option<IntegrationOutcome>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerReleaseSource {
    ReleaseReply,
    QueryLease,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveredBrokerRelease {
    pub helper_lease: LeaseId,
    pub broker_lease: LeaseId,
    pub helper_intent: String,
    pub source: BrokerReleaseSource,
}
pub fn settled_original_broker_state(
    state: &theseus_native_protocol::v2::broker::BrokerState,
) -> bool {
    state.original_retired()
}
/// An effect terminal is knowledge about this original action, not its lifetime.
/// Check it before admitting any recovered state or issuing further control.
pub fn validate_original_broker_effect(
    action: &IntegrationActionRecord,
    state: &theseus_native_protocol::v2::broker::BrokerState,
) -> Result<()> {
    ensure!(
        matches!(action.request, IntegrationRequest::Desktop { .. }),
        "broker state borrowed service effect authority"
    );
    if let IntegrationRequest::Desktop { request, .. } = &action.request
        && matches!(
            request.action,
            theseus_native_protocol::v2::broker::DesktopAction::Activate { .. }
        )
    {
        ensure!(
            request.owned_lifetime_definition()
                == state.owned_lifetime.as_ref().map(|s| s.definition),
            "recovered owned lifetime changed the original Activate declaration"
        );
    }
    validate_broker_effect_terminal(action.terminal.as_ref(), state)
}
pub fn validate_broker_effect_terminal(
    terminal: Option<&IntegrationOutcome>,
    state: &theseus_native_protocol::v2::broker::BrokerState,
) -> Result<()> {
    use theseus_native_protocol::v2::broker::BrokerEffect;
    state.validate()?;
    match terminal {
        Some(IntegrationOutcome::Complete { receipt: known }) => ensure!(
            matches!(&state.effect,BrokerEffect::Receipt {receipt} if receipt.acknowledged && &receipt.receipt_digest==known),
            "recovered broker effect contradicts original acknowledged terminal"
        ),
        Some(IntegrationOutcome::Failed { .. }) => ensure!(
            matches!(&state.effect,BrokerEffect::Receipt {receipt} if !receipt.acknowledged),
            "recovered broker effect contradicts original failed terminal"
        ),
        Some(IntegrationOutcome::Unknown { .. }) => ensure!(
            matches!(state.effect, BrokerEffect::Unknown),
            "recovered broker effect contradicts original cached Unknown"
        ),
        None => {}
    }
    Ok(())
}
/// Original report observations precede recovery. They retain independently
/// bound irreversible facts even when the recovery journal has no history yet.
pub fn validate_original_broker_report_progress(
    report: &NativeIntegrationReport,
    action: &IntegrationActionRecord,
    next: &theseus_native_protocol::v2::broker::BrokerState,
) -> Result<()> {
    for observation in report
        .integrations
        .iter()
        .filter(|s| s.ownership_id == action.ownership_id)
        .flat_map(|s| &s.desktops)
        .filter(|d| d.slot == action.slot)
        .flat_map(|d| &d.observations)
        .filter(|o| {
            o.binding.ownership_id == action.ownership_id
                && o.binding.lease == action.lease
                && o.binding.action.as_ref() == Some(&action.operation)
                && o.binding.binding_digest == action.prior_binding_digest
        })
    {
        if let Some(previous) = &observation.broker_state {
            previous.validate()?;
            validate_broker_progress(previous, next)?;
        }
    }
    Ok(())
}
pub fn validate_broker_progress(
    previous: &theseus_native_protocol::v2::broker::BrokerState,
    next: &theseus_native_protocol::v2::broker::BrokerState,
) -> Result<()> {
    use theseus_native_protocol::v2::broker::*;
    next.validate()?;
    match (&previous.owned_lifetime, &next.owned_lifetime) {
        (Some(old), Some(new)) => new.validate_progress_from(old)?,
        (Some(_), None) => anyhow::bail!("original owned lifetime projection disappeared"),
        (None, Some(_)) => anyhow::bail!("owned lifetime invented after original initial state"),
        _ => {}
    }
    match (&previous.native_lifetime, &next.native_lifetime) {
        (Some(old), Some(new)) => new.validate_progress_from(old)?,
        (Some(_), None) => anyhow::bail!("original native lifetime projection disappeared"),
        (None, Some(_)) => ensure!(
            !previous.quarantined
                && previous.owner != BrokerOwner::Lost
                && !matches!(previous.effect, BrokerEffect::Receipt { .. }),
            "native authority invented after original terminal or lost owner"
        ),
        _ => {}
    }
    ensure!(
        !matches!(previous.effect, BrokerEffect::Receipt { .. }) || previous.effect == next.effect,
        "original broker effect receipt changed"
    );
    ensure!(
        !previous.quarantined || next.quarantined,
        "original broker quarantine cleared"
    );
    ensure!(
        !previous.cleanup_complete || next.cleanup_complete,
        "original broker cleanup fact regressed"
    );
    ensure!(
        !matches!(
            previous.primary,
            BrokerPrimary::Exited | BrokerPrimary::NotCreated
        ) || next.primary == previous.primary,
        "original primary exit/noncreation fact regressed"
    );
    ensure!(
        !matches!(
            previous.descendants,
            BrokerDescendants::Joined | BrokerDescendants::NotCreated
        ) || next.descendants == previous.descendants,
        "original scope join/noncreation fact regressed"
    );
    ensure!(
        !matches!(previous.owner, BrokerOwner::Lost | BrokerOwner::Settled)
            || next.owner != BrokerOwner::Retained,
        "lost or settled original native owner was reconstructed"
    );
    Ok(())
}
/// Saved knowledge about an exact original action, never execution authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginalQueryView {
    CachedTerminal,
    NonterminalUnknown,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub broker_effect_baselines: Vec<OriginalBrokerEffectBaseline>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub broker_observations: Vec<RecoveredBrokerObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker_release: Option<RecoveredBrokerRelease>,
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
impl IntegrationScenarioEvidence {
    pub fn complete(&self) -> bool {
        if self.owned_lifetime.is_some()
            || self
                .desktops
                .iter()
                .flat_map(|d| &d.observations)
                .filter_map(|o| o.broker_state.as_ref())
                .any(|s| s.owned_lifetime.is_some())
        {
            return super::owned_lifetime::scenario_complete(self);
        }
        let Some(native) = &self.native_lifetime else {
            let native_observed=self.desktops.iter().flat_map(|d|&d.observations).filter_map(|o|o.broker_state.as_ref()).any(|s|s.native_lifetime.is_some() || matches!(&s.effect,theseus_native_protocol::v2::broker::BrokerEffect::Receipt {receipt} if receipt.evidence==theseus_native_protocol::v2::broker::DesktopEvidence::NativeActivation));
            return !native_observed && self.dispositions.complete();
        };
        use theseus_native_protocol::v2::broker::*;
        if native.state.validate().is_err() {
            return false;
        }
        let d = &self.dispositions;
        let complete = |d: &Disposition| matches!(d, Disposition::Complete { .. });
        [
            &d.helper_worker,
            &d.registration,
            &d.manager_action,
            &d.broker,
        ]
        .into_iter()
        .all(complete)
            && match native.state.primary {
                NativePrimaryState::Unavailable
                    if native.state.authority == NativeLifetimeAuthority::ManagerInstance =>
                {
                    matches!(d.primary, Disposition::NotStarted)
                }
                NativePrimaryState::Exited => complete(&d.primary),
                _ => false,
            }
            && native.state.descendants == NativeTreeState::Unavailable
            && matches!(d.descendant_scope, Disposition::NotStarted)
            && self.desktops.iter().all(|desktop| {
                let mut latest = BTreeMap::new();
                for o in &desktop.observations {
                    if let (Some(action), Some(state)) = (&o.binding.action, &o.broker_state) {
                        latest.insert(action, state);
                    }
                }
                latest
                    .values()
                    .filter(|s| s.native_lifetime.is_some())
                    .all(|s| s.original_retired())
            })
            && self.desktops.iter().any(|desktop| {
                desktop.actions.contains(&native.original)
                    && desktop.observations.iter().any(|o| {
                        o.binding.action.as_ref() == Some(&native.original.operation)
                            && o.broker_state.as_ref().is_some_and(|s| {
                                s.native_lifetime.as_ref() == Some(&native.state)
                                    && s.original_retired()
                            })
                    })
            })
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
            super::owned_lifetime::validate_report_scenario(&self.requirements, integration)?;
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
                super::owned_lifetime::validate_recovered_observation(
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
    for integration in &report.integrations {
        super::owned_lifetime::validate_pair_projection(integration, journal)?;
        for desktop in &integration.desktops {
            for observation in &desktop.observations {
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
                            anyhow::anyhow!(
                                "desktop observation lacks original published activation"
                            )
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
                    if let Some(state) = &observation.broker_state
                        && (request.owned_lifetime_definition().is_some()
                            || state.owned_lifetime.is_some())
                    {
                        let broker = journal
                            .broker_bindings
                            .iter()
                            .find(|b| {
                                b.ownership_id == binding.ownership_id
                                    && b.helper_lease == binding.lease
                                    && b.broker_lease.as_ref() == Some(&request.broker_lease)
                            })
                            .ok_or_else(|| {
                                anyhow::anyhow!("owned lifetime original broker mapping missing")
                            })?;
                        super::owned_lifetime::validate_original_observation(
                            request,
                            state,
                            report.requirements.desktop.as_ref(),
                            &broker.original.expected_header.os,
                        )?;
                        super::owned_lifetime::validate_report_effect(action, state)?;
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
                            .ok_or_else(|| {
                                anyhow::anyhow!("native lifetime original broker mapping missing")
                            })?;
                        let required = report.requirements.desktop.as_ref().ok_or_else(|| {
                            anyhow::anyhow!("native lifetime requirements missing")
                        })?;
                        ensure!(required.modes==[DesktopMode::NativeActivation] && native.required==required.lifetime.native() && matches!(request.action,theseus_native_protocol::v2::broker::DesktopAction::Activate {mode:theseus_native_protocol::v2::broker::DesktopMode::NativeActivation,..}),"native lifetime borrowed another mode/requirement");
                        native.validate_platform(&broker.original.expected_header.os)?;
                    }
                    if let Some(ready) = &observation.readiness {
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
                            .ok_or_else(|| {
                                anyhow::anyhow!("desktop readiness original broker mapping missing")
                            })?;
                        ready.validate_desktop_peer(
                            &broker.original.expected_header.broker_principal,
                        )?;
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
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "desktop readiness lacks exact original published probe"
                                )
                            })?;
                        if ready.state == theseus_native_protocol::v2::ReadinessState::Ready {
                            ensure!(
                                matches!(&probe.completion,Disposition::Complete {receipt} if Some(receipt)==ready.response_digest.as_ref()),
                                "desktop Ready response lacks exact completed private receipt"
                            );
                        }
                    }
                }
            }
        }
    }
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

/// Capture currently known original terminals without adding action authority.
/// A cached original query can precede the private terminal-completion barrier.
pub fn original_broker_effect_baselines(
    journal: &NativeIntegrationJournal,
    owner: &str,
) -> Result<Vec<OriginalBrokerEffectBaseline>> {
    journal
        .integration_actions
        .iter()
        .filter(|a| {
            a.ownership_id == owner && matches!(a.request, IntegrationRequest::Desktop { .. })
        })
        .map(|action| {
            let original = ActionReference {
                operation: action.operation.clone(),
                request_digest: action.request_digest.clone(),
            };
            let mut terminal = action.terminal.clone();
            for query in journal
                .integration_recoveries
                .iter()
                .filter(|r| r.ownership_id == owner)
                .flat_map(|r| &r.queries)
                .filter(|q| q.original == original && q.view == OriginalQueryView::CachedTerminal)
            {
                if let Some(known) = &terminal {
                    ensure!(
                        known == &query.outcome,
                        "original desktop terminal differs from prior cached query"
                    );
                } else {
                    terminal = Some(query.outcome.clone());
                }
            }
            Ok(OriginalBrokerEffectBaseline { original, terminal })
        })
        .collect()
}
/// Cached query provenance cannot disagree with an authenticated original
/// receipt. Earlier unavailable effects remain historical Unknown samples.
pub fn validate_original_query_broker_receipts(
    journal: &NativeIntegrationJournal,
    owner: &str,
    query: &OriginalQueryOutcome,
) -> Result<()> {
    if query.view != OriginalQueryView::CachedTerminal {
        return Ok(());
    }
    for observation in journal
        .integration_recoveries
        .iter()
        .filter(|r| r.ownership_id == owner)
        .flat_map(|r| &r.broker_observations)
        .filter(|o| o.original == query.original)
    {
        if matches!(
            observation.state.effect,
            theseus_native_protocol::v2::broker::BrokerEffect::Receipt { .. }
        ) {
            validate_broker_effect_terminal(Some(&query.outcome), &observation.state)?;
        }
    }
    Ok(())
}
pub fn original_desktop_scope_settled(
    journal: &NativeIntegrationJournal,
    owner: &str,
    slot: &ResourceSlot,
) -> bool {
    let actions = journal
        .integration_actions
        .iter()
        .filter(|a| {
            a.ownership_id == owner
                && &a.slot == slot
                && matches!(a.request, IntegrationRequest::Desktop { .. })
        })
        .collect::<Vec<_>>();
    !actions.is_empty()
        && actions.iter().all(|action| {
            journal
                .integration_recoveries
                .iter()
                .rev()
                .filter(|r| r.ownership_id == owner)
                .flat_map(|r| r.broker_observations.iter().rev())
                .find(|o| {
                    o.original.operation == action.operation
                        && o.original.request_digest == action.request_digest
                })
                .is_some_and(|o| settled_original_broker_state(&o.state))
        })
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

impl IntegrationActionRecord {
    /// Cross-check duplicated diagnostic IDs against the exact canonical request
    /// before those IDs can select an original owner for Query/Cancel.
    pub fn validate_identity(&self) -> Result<()> {
        let (lease, operation, slot, prior_binding_digest) = match &self.request {
            IntegrationRequest::Service {
                lease,
                operation,
                slot,
                prior_binding_digest,
                ..
            }
            | IntegrationRequest::Desktop {
                lease,
                operation,
                slot,
                prior_binding_digest,
                ..
            } => (lease, operation, slot, prior_binding_digest),
        };
        ensure!(
            lease == &self.lease
                && operation == &self.operation
                && slot == &self.slot
                && prior_binding_digest == &self.prior_binding_digest,
            "integration action differs from canonical original identity"
        );
        ensure!(
            theseus_native_protocol::v2::request_digest(&self.request.wire()?)?.to_hex()
                == self.request_digest,
            "integration canonical request digest differs"
        );
        if let IntegrationRequest::Desktop { request, .. } = &self.request {
            let identity = request.identity()?;
            let expected: BrokerOperationIdentity =
                serde_json::from_value(serde_json::to_value(identity)?)?;
            ensure!(
                self.broker_identity.as_ref() == Some(&expected),
                "desktop original broker identity changed"
            );
        } else {
            ensure!(
                self.broker_identity.is_none(),
                "service action borrowed desktop identity"
            );
        }
        Ok(())
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
            Self::Desktop {
                lease,
                operation,
                slot,
                prior_binding_digest,
                mode,
                request,
            } => {
                request.validate()?;
                ensure!(
                    &request.helper_lease == lease
                        && &request.helper_operation == operation
                        && request.slot.as_str() == slot.as_str()
                        && &request.prior_binding_digest == prior_binding_digest,
                    "desktop canonical request identity changed"
                );
                if let v2::broker::DesktopAction::Activate {
                    mode: wire_mode, ..
                } = &request.action
                {
                    ensure!(
                        matches!(
                            (mode, wire_mode),
                            (
                                DesktopMode::OwnedExecutable,
                                v2::broker::DesktopMode::OwnedExecutable
                            ) | (
                                DesktopMode::NativeActivation,
                                v2::broker::DesktopMode::NativeActivation
                            )
                        ),
                        "desktop mode changed"
                    );
                }
                Ok(v2::Request::DesktopAction {
                    request: request.clone(),
                })
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

#[cfg(test)]
mod native_desktop_mode_tests {
    use super::*;
    use theseus_native_protocol::v2::broker::*;
    fn native_state(authority: NativeLifetimeAuthority) -> BrokerState {
        let manager = authority == NativeLifetimeAuthority::ManagerInstance;
        BrokerState {
            owned_lifetime: None,
            effect: BrokerEffect::Receipt {
                receipt: BrokerEffectReceipt {
                    evidence:
                        theseus_native_protocol::v2::broker::DesktopEvidence::NativeActivation,
                    acknowledged: true,
                    receipt_digest: "ab".repeat(32),
                },
            },
            owner: BrokerOwner::Settled,
            primary: if manager {
                BrokerPrimary::Unknown
            } else {
                BrokerPrimary::Exited
            },
            descendants: BrokerDescendants::Unknown,
            cleanup_complete: false,
            quarantined: false,
            native_lifetime: Some(NativeLifetimeState {
                authority,
                required: if manager {
                    NativeLifetimeRequirement::ActivationInstanceTerminated
                } else {
                    NativeLifetimeRequirement::OwnedPrimaryExited
                },
                instance: NativeInstanceState::Terminated,
                primary: if manager {
                    NativePrimaryState::Unavailable
                } else {
                    NativePrimaryState::Exited
                },
                descendants: NativeTreeState::Unavailable,
                local_resources: NativeLocalState::Retired,
            }),
        }
    }
    pub(super) fn observation(state: BrokerState) -> BoundDesktopObservation {
        BoundDesktopObservation {
            generation: None,
            binding: ObservationBinding {
                id: "native-observation".into(),
                run_id: "run".into(),
                scenario_id: "clean".into(),
                ownership_id: "owner".into(),
                lease: "01".repeat(16),
                slot: ResourceSlot::new("desktop").unwrap(),
                request_id: "query".into(),
                action: Some("02".repeat(16)),
                binding_digest: "03".repeat(32),
                source_profile: crate::native::integration::PROFILE.into(),
                source_version: "fixture".into(),
                phase: IntegrationPhase::BeforeUninstall,
                definition: None,
                sequence: 1,
                started_ms: 0,
                finished_ms: 1,
                complete: true,
            },
            evidence: Some(theseus_native_protocol::v2::broker::DesktopEvidence::NativeActivation),
            broker_state: Some(state),
            readiness: None,
            entry_valid: None,
            entry_present: IntegrationProvenance::Unavailable {
                reason: "no entry inspection".into(),
            },
            activated: IntegrationProvenance::Unavailable {
                reason: "separate effect receipt".into(),
            },
            primary_exited: IntegrationProvenance::Unavailable {
                reason: "manager-only".into(),
            },
            descendant_scope_settled: IntegrationProvenance::Unavailable {
                reason: "native tree unavailable".into(),
            },
        }
    }
    #[test]
    fn native_scenario_excludes_only_committed_unavailable_axes_and_preserves_old_bytes() {
        let mut scenario = crate::native::integration::view::tests::report()
            .integrations
            .remove(0);
        let old = serde_json::to_value(&scenario).unwrap();
        assert!(old.get("native_lifetime").is_none());
        let state = native_state(NativeLifetimeAuthority::ManagerInstance);
        let bound = observation(state.clone());
        let reference = ActionReference {
            operation: bound.binding.action.clone().unwrap(),
            request_digest: "04".repeat(32),
        };
        scenario.desktops = vec![super::DesktopEvidence {
            slot: bound.binding.slot.clone(),
            actions: vec![reference.clone()],
            observations: vec![bound],
        }];
        scenario.native_lifetime = Some(NativeDesktopLifetimeProjection {
            original: reference,
            state: state.native_lifetime.unwrap(),
        });
        let done = || Disposition::Complete {
            receipt: "separate required domain completed".into(),
        };
        scenario.dispositions.helper_worker = done();
        scenario.dispositions.registration = done();
        scenario.dispositions.manager_action = done();
        scenario.dispositions.broker = done();
        scenario.dispositions.primary = Disposition::NotStarted;
        scenario.dispositions.descendant_scope = Disposition::NotStarted;
        assert!(scenario.complete());
        assert!(
            !scenario.dispositions.complete(),
            "legacy strong completion is unchanged"
        );
        for failed in [
            Disposition::Unknown {
                reason: "historically owned scope lost".into(),
            },
            Disposition::Failed {
                reason: "owned scope failed".into(),
            },
        ] {
            let mut bad = scenario.clone();
            bad.dispositions.descendant_scope = failed.clone();
            assert!(!bad.complete());
            let mut bad = scenario.clone();
            bad.dispositions.primary = failed;
            assert!(!bad.complete());
        }
        let mut wrong = scenario.clone();
        wrong
            .native_lifetime
            .as_mut()
            .unwrap()
            .original
            .request_digest = "05".repeat(32);
        assert!(!wrong.complete());
        let mut local = scenario;
        local
            .native_lifetime
            .as_mut()
            .unwrap()
            .state
            .local_resources = NativeLocalState::Retained;
        assert!(!local.complete());
    }
    #[test]
    fn native_saved_view_names_unavailable_axes_and_rejects_borrowed_owned_mode() {
        let mut report = crate::native::integration::view::tests::report();
        let required = DesktopRequirement {
            resource: ResourceSlot::new("desktop").unwrap(),
            modes: vec![super::DesktopMode::NativeActivation],
            readiness: ReadinessRequirement {
                kind: ReadyKind::LocalChallenge,
                protocol: ReadyProtocol::TheseusFixtureReadyV1,
                deadline_ms: 1000,
            },
            lifetime: LifetimeRequirement::ActivationInstanceTerminated,
        };
        report.requirements = IntegrationRequirements {
            service: None,
            desktop: Some(required),
        };
        report.compiled_requirements_digest = report.requirements.digest().unwrap();
        let state = native_state(NativeLifetimeAuthority::ManagerInstance);
        let mut bound = observation(state.clone());
        bound.binding.run_id = report.run_id.clone();
        let reference = ActionReference {
            operation: bound.binding.action.clone().unwrap(),
            request_digest: "04".repeat(32),
        };
        let scenario = &mut report.integrations[0];
        scenario.services.clear();
        scenario.checks.clear();
        scenario.desktops = vec![super::DesktopEvidence {
            slot: bound.binding.slot.clone(),
            actions: vec![reference.clone()],
            observations: vec![bound],
        }];
        scenario.native_lifetime = Some(NativeDesktopLifetimeProjection {
            original: reference,
            state: state.native_lifetime.unwrap(),
        });
        let page = crate::native::integration::view::evidence_page(
            &report,
            crate::native::integration::view::Selector::Desktop,
            0,
            64,
        )
        .unwrap();
        let native = &page.records[0]["native_lifetime"]["state"];
        assert_eq!(native["primary"], "unavailable");
        assert_eq!(native["descendants"], "unavailable");
        assert_eq!(native["local_resources"], "retired");
        report.requirements.desktop.as_mut().unwrap().modes =
            vec![super::DesktopMode::OwnedExecutable];
        report.compiled_requirements_digest = report.requirements.digest().unwrap();
        assert!(
            report.validate().is_err(),
            "native receipt cannot satisfy an owned saved report"
        );
    }
    #[test]
    fn public_two_generations_never_serialize_original_activation_nonce() {
        use theseus_native_protocol::v2;
        let prior = v2::broker::BrokerOperationIdentity {
            helper_lease: "01".repeat(16),
            helper_operation: "02".repeat(16),
            broker_lease: "03".repeat(16),
            request_digest: "04".repeat(32),
            binding_digest: "05".repeat(32),
            activation_nonce: "06".repeat(16),
        };
        let witness = |operation: &str, program: &str| {
            let program = v2::ProgramId::new(program).unwrap();
            let lease = v2::LeaseId::parse(&prior.helper_lease).unwrap();
            let op = v2::OperationId::parse(operation).unwrap();
            DesktopSutRunWitness {
                lease: prior.helper_lease.clone(),
                operation: operation.into(),
                request_digest: v2::request_digest(&v2::Request::Run {
                    lease,
                    operation: op,
                    program: program.clone(),
                })
                .unwrap()
                .to_hex(),
                program,
            }
        };
        let first = DesktopActivationTransition {
            definition: v2::DesktopDefinitionKey::CONFIGURED_TARGET,
            tuple_digest: "08".repeat(32),
            sut: witness(&"07".repeat(16), "configure"),
            previous_activation: None,
        };
        let second = DesktopActivationTransition {
            definition: first.definition,
            tuple_digest: first.tuple_digest.clone(),
            sut: witness(&"09".repeat(16), "install_target"),
            previous_activation: Some(prior.clone()),
        };
        let mut a = observation(native_state(NativeLifetimeAuthority::ManagerInstance));
        a.generation = Some(desktop_generation_evidence(&first));
        let mut b = a.clone();
        b.generation = Some(desktop_generation_evidence(&second));
        let public = serde_json::to_string(&[a, b]).unwrap();
        assert!(
            !public.contains(&prior.activation_nonce),
            "public generation2 leaked private original activation_nonce"
        );
        assert!(!public.contains("activation_nonce"));
        assert!(public.contains(&second.sut.operation));
    }
    #[test]
    fn native_manager_termination_satisfies_only_its_mandatory_original_lifetime() {
        let state = native_state(NativeLifetimeAuthority::ManagerInstance);
        state.validate().unwrap();
        assert!(state.original_retired());
        let observed = observation(state.clone());
        assert!(desktop_predicate_satisfied(
            &observed,
            &IntegrationPredicate::Lifetime(LifetimeRequirement::ActivationInstanceTerminated)
        ));
        assert!(!desktop_predicate_satisfied(
            &observed,
            &IntegrationPredicate::Lifetime(LifetimeRequirement::OwnedPrimaryExited)
        ));
        assert!(!desktop_predicate_satisfied(
            &observed,
            &IntegrationPredicate::Lifetime(LifetimeRequirement::OwnedProcessTreeSettled)
        ));
        assert!(settled_original_broker_state(&state));
    }
    #[test]
    fn native_child_and_handle_use_original_projection_and_never_legacy_tree_retirement() {
        for authority in [
            NativeLifetimeAuthority::OriginalChild,
            NativeLifetimeAuthority::OriginalPrimaryHandle,
        ] {
            let state = native_state(authority);
            state.validate().unwrap();
            let observed = observation(state.clone());
            assert!(desktop_predicate_satisfied(
                &observed,
                &IntegrationPredicate::Lifetime(LifetimeRequirement::OwnedPrimaryExited)
            ));
            assert!(settled_original_broker_state(&state));
            let mut missing = observed.clone();
            missing.broker_state.as_mut().unwrap().native_lifetime = None;
            assert!(
                !desktop_predicate_satisfied(
                    &missing,
                    &IntegrationPredicate::Lifetime(LifetimeRequirement::OwnedPrimaryExited)
                ),
                "native evidence cannot borrow a legacy primary fact"
            );
            let mut forged = observed;
            let s = forged.broker_state.as_mut().unwrap();
            s.primary = BrokerPrimary::Exited;
            s.descendants = BrokerDescendants::Joined;
            s.cleanup_complete = true;
            assert!(!desktop_predicate_satisfied(
                &forged,
                &IntegrationPredicate::Lifetime(LifetimeRequirement::OwnedProcessTreeSettled)
            ));
        }
    }
    #[test]
    fn native_original_progress_cannot_drop_projection_or_redeclare_unknown_unavailable() {
        let previous = native_state(NativeLifetimeAuthority::ManagerInstance);
        let mut missing = previous.clone();
        missing.native_lifetime = None;
        assert!(validate_broker_progress(&previous, &missing).is_err());
        let mut unknown = previous.clone();
        unknown.native_lifetime.as_mut().unwrap().primary = NativePrimaryState::Unknown;
        assert!(validate_broker_progress(&unknown, &previous).is_err());
        let mut regressed = previous.clone();
        regressed.native_lifetime.as_mut().unwrap().local_resources = NativeLocalState::Retained;
        assert!(validate_broker_progress(&previous, &regressed).is_err());
    }
}

#[cfg(test)]
mod desktop_ordinary_contract_tests {
    use super::*;
    fn requirement() -> DesktopRequirement {
        DesktopRequirement {
            resource: ResourceSlot::new("desktop").unwrap(),
            modes: vec![DesktopMode::OwnedExecutable],
            readiness: ReadinessRequirement {
                kind: ReadyKind::LocalChallenge,
                protocol: ReadyProtocol::TheseusFixtureReadyV1,
                deadline_ms: 1000,
            },
            lifetime: LifetimeRequirement::OwnedProcessTreeSettled,
        }
    }
    #[test]
    fn desktop_ordinary_checks_keep_legacy_clean_and_distinct_original_generations() {
        let r = requirement();
        assert_eq!(
            desktop_scenario_check_specs(&r, ScenarioKind::Clean),
            desktop_check_specs(&r)
        );
        let upgrade = desktop_scenario_check_specs(&r, ScenarioKind::Upgrade);
        assert!(
            upgrade
                .iter()
                .any(|(_, phase, p, _)| *phase == IntegrationPhase::BeforeUpgrade
                    && *p == IntegrationPredicate::Lifetime(r.lifetime))
        );
        assert!(upgrade.iter().any(|(_, phase, p, _)| *phase
            == IntegrationPhase::TargetInstalledAfterUpgrade
            && *p == IntegrationPredicate::ApplicationReady));
        let reinstall = desktop_scenario_check_specs(&r, ScenarioKind::Reinstall);
        for phase in [
            IntegrationPhase::ExerciseBeforeReinstall,
            IntegrationPhase::ExerciseAfterReinstall,
        ] {
            assert!(reinstall.iter().any(|(_, p, predicate, _)| *p == phase
                && *predicate == IntegrationPredicate::ApplicationReady));
        }
        assert!(reinstall.iter().any(|(_, phase, p, _)| *phase
            == IntegrationPhase::BeforeReinstall
            && *p == IntegrationPredicate::Lifetime(r.lifetime)));
        assert_eq!(
            desktop_phase_definition(ScenarioKind::Upgrade, IntegrationPhase::Configured),
            Some(DefinitionKey {
                role: DefinitionRole::Configured,
                artifact: ArtifactRole::Base
            })
        );
        assert_eq!(
            desktop_phase_definition(
                ScenarioKind::Upgrade,
                IntegrationPhase::TargetInstalledAfterUpgrade
            ),
            Some(DefinitionKey {
                role: DefinitionRole::Configured,
                artifact: ArtifactRole::Target
            })
        );
        assert_eq!(
            desktop_phase_definition(ScenarioKind::Reinstall, IntegrationPhase::BeforeReinstall),
            Some(DefinitionKey {
                role: DefinitionRole::Configured,
                artifact: ArtifactRole::Target
            })
        );
    }
}

#[cfg(test)]
mod desktop_generation_sut_tests {
    use super::*;
    use theseus_native_protocol::v2;
    #[test]
    fn generation_requires_original_successful_sut_lease_program_digest_and_measured_status() {
        let catalog = CatalogCommand {
            id: "install_target".into(),
            program: CatalogProgram::InstallTarget,
            expected_status: NativeStatus::PosixExit { code: 0 },
            stdout_literal: None,
        };
        let mut operation = NativeOperation {
            ownership_id: "owner".into(),
            lease: "01".repeat(16),
            operation_id: "02".repeat(16),
            request_digest: String::new(),
            command: catalog.clone(),
            completion: Disposition::Complete {
                receipt: "original accepted Run completed".into(),
            },
        };
        operation.request_digest = v2::request_digest(&operation.request().unwrap())
            .unwrap()
            .to_hex();
        let witness = v2::broker::DesktopSutRunWitness {
            lease: operation.lease.clone(),
            operation: operation.operation_id.clone(),
            request_digest: operation.request_digest.clone(),
            program: catalog.program.program_id(),
        };
        let measured = NativeCommandEvidence {
            command: catalog,
            operation_id: operation.operation_id.clone(),
            request_digest: operation.request_digest.clone(),
            status: Some(NativeStatus::PosixExit { code: 0 }),
            stdout: String::new(),
            stdout_complete: true,
            stdout_utf8: true,
            stderr_complete: true,
            stdout_log: None,
            stderr_log: None,
            failure: None,
        };
        validate_desktop_sut_generation("owner", &operation.lease, &witness, &operation, &measured)
            .unwrap();
        let mut foreign = witness.clone();
        foreign.operation = "03".repeat(16);
        assert!(
            validate_desktop_sut_generation(
                "owner",
                &operation.lease,
                &foreign,
                &operation,
                &measured
            )
            .is_err()
        );
        let mut lost = operation.clone();
        lost.completion = Disposition::Unknown {
            reason: "original completion lost".into(),
        };
        assert!(
            validate_desktop_sut_generation("owner", &operation.lease, &witness, &lost, &measured)
                .is_err()
        );
        let mut failed = measured.clone();
        failed.status = Some(NativeStatus::PosixExit { code: 1 });
        assert!(
            validate_desktop_sut_generation(
                "owner",
                &operation.lease,
                &witness,
                &operation,
                &failed
            )
            .is_err()
        );
        assert!(
            validate_desktop_sut_generation(
                "other-owner",
                &operation.lease,
                &witness,
                &operation,
                &measured
            )
            .is_err()
        );
        assert!(
            validate_desktop_sut_generation(
                "owner",
                &"04".repeat(16),
                &witness,
                &operation,
                &measured
            )
            .is_err()
        );
        let mut changed = measured;
        changed.command.program = CatalogProgram::Configure;
        assert!(
            validate_desktop_sut_generation(
                "owner",
                &operation.lease,
                &witness,
                &operation,
                &changed
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod desktop_generation_presence_tests {
    use super::*;
    use theseus_native_protocol::v2::{self, broker};

    // Pure saved DTOs: no application, broker, manager, or helper is executed.
    pub(super) fn pair(
        scenario: ScenarioKind,
        activated: bool,
    ) -> (NativeIntegrationReport, NativeIntegrationJournal) {
        let mut report = crate::native::integration::view::tests::report();
        let slot = ResourceSlot::new("desktop").unwrap();
        report.requirements = IntegrationRequirements {
            service: None,
            desktop: Some(DesktopRequirement {
                resource: slot.clone(),
                modes: vec![DesktopMode::OwnedExecutable],
                readiness: ReadinessRequirement {
                    kind: ReadyKind::LocalChallenge,
                    protocol: ReadyProtocol::TheseusFixtureReadyV1,
                    deadline_ms: 1000,
                },
                lifetime: LifetimeRequirement::OwnedProcessTreeSettled,
            }),
        };
        report.compiled_requirements_digest = report.requirements.digest().unwrap();
        report.selected_scenarios = vec![scenario];
        report.catalog.scenarios[0].scenario = scenario;
        report.catalog.scenarios[0].ownership_id = "16".repeat(16);
        report.integrations[0].ownership_id = "16".repeat(16);
        let command = CatalogCommand {
            id: if scenario == ScenarioKind::Clean {
                "configure"
            } else {
                "install_target"
            }
            .into(),
            program: if scenario == ScenarioKind::Clean {
                CatalogProgram::Configure
            } else {
                CatalogProgram::InstallTarget
            },
            expected_status: NativeStatus::PosixExit { code: 0 },
            stdout_literal: None,
        };
        let mut operation = NativeOperation {
            ownership_id: "16".repeat(16),
            lease: "01".repeat(16),
            operation_id: "02".repeat(16),
            request_digest: String::new(),
            command: command.clone(),
            completion: Disposition::Complete {
                receipt: "original successful Run".into(),
            },
        };
        operation.request_digest = v2::request_digest(&operation.request().unwrap())
            .unwrap()
            .to_hex();
        let generation = broker::DesktopActivationTransition {
            definition: v2::DesktopDefinitionKey::CONFIGURED_TARGET,
            tuple_digest: "03".repeat(32),
            previous_activation: None,
            sut: broker::DesktopSutRunWitness {
                lease: operation.lease.clone(),
                operation: operation.operation_id.clone(),
                request_digest: operation.request_digest.clone(),
                program: command.program.program_id(),
            },
        };
        report.catalog.scenarios[0].stages = vec![NativeStage {
            name: "original SUT".into(),
            step: Some(match scenario {
                ScenarioKind::Clean => crate::model::StepName::Configure,
                ScenarioKind::Upgrade => crate::model::StepName::Upgrade,
                _ => crate::model::StepName::Reinstall,
            }),
            checkpoint: None,
            outcome: crate::model::StageOutcome::Passed,
            commands: vec![NativeCommandEvidence {
                command,
                operation_id: operation.operation_id.clone(),
                request_digest: operation.request_digest.clone(),
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
        }];
        let header: broker::IntegrationBrokerHeader = serde_json::from_value(serde_json::json!({
            "profile":"native_integration_broker_v1","binding":"04".repeat(16),"manifest_digest":report.manifest_digest,"workspace_marker":"05".repeat(16),"os":"linux","target_id":"original-target","ownership_id":"16".repeat(16),"provider_epoch":"06".repeat(32),"session_id":"original-session","session_epoch":"07".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},"allowed_modes":["owned_executable"]
        })).unwrap();
        let mut request: broker::DesktopRequest = serde_json::from_value(serde_json::json!({
            "helper_lease":operation.lease,"helper_operation":"08".repeat(16),"broker_lease":"09".repeat(16),"binding_digest":header.digest().unwrap().to_hex(),"activation_nonce":"0a".repeat(16),"slot":"desktop","prior_binding_digest":"0b".repeat(32),"action":{"kind":"activate","transition":generation,"mode":"owned_executable","entry_digest":"0c".repeat(32),"expectation_digest":"0d".repeat(32),"close_binding":{"operation":"0e".repeat(16),"action_digest":"00".repeat(32),"nonce":"0f".repeat(16)}}
        })).unwrap();
        let close_digest = request.close_request().unwrap().digest().unwrap().to_hex();
        if let broker::DesktopAction::Activate { close_binding, .. } = &mut request.action {
            close_binding.action_digest = close_digest;
        }
        let original = broker::BrokerBind {
            ownership_id: "16".repeat(16),
            helper_lease: operation.lease.clone(),
            helper_operation: "10".repeat(16),
            slot: v2::ResourceSlot::new("desktop").unwrap(),
            broker_intent: "11".repeat(16),
            expected_header: header.clone(),
        };
        let unavailable = || IntegrationProvenance::Unavailable {
            reason: "not sampled by this observation".into(),
        };
        let state = broker::BrokerState {
            owned_lifetime: None,
            native_lifetime: None,
            effect: broker::BrokerEffect::Receipt {
                receipt: broker::BrokerEffectReceipt {
                    evidence: broker::DesktopEvidence::SourceOwnedExecutable,
                    acknowledged: true,
                    receipt_digest: "12".repeat(32),
                },
            },
            owner: broker::BrokerOwner::Settled,
            primary: broker::BrokerPrimary::Exited,
            descendants: broker::BrokerDescendants::Joined,
            cleanup_complete: true,
            quarantined: false,
        };
        let observation = BoundDesktopObservation {
            generation: Some(desktop_generation_evidence(&generation)),
            entry_valid: Some(true),
            broker_state: activated.then_some(state),
            readiness: None,
            evidence: activated.then_some(broker::DesktopEvidence::SourceOwnedExecutable),
            binding: ObservationBinding {
                id: "original-observation".into(),
                run_id: report.run_id.clone(),
                scenario_id: "clean".into(),
                ownership_id: "16".repeat(16),
                lease: operation.lease.clone(),
                slot: slot.clone(),
                request_id: "original-query".into(),
                action: Some(if activated {
                    request.helper_operation.clone()
                } else {
                    operation.operation_id.clone()
                }),
                binding_digest: request.prior_binding_digest.clone(),
                source_profile: super::super::PROFILE.into(),
                source_version: "fixture".into(),
                phase: match scenario {
                    ScenarioKind::Clean => IntegrationPhase::Configured,
                    ScenarioKind::Upgrade => IntegrationPhase::TargetInstalledAfterUpgrade,
                    _ => IntegrationPhase::Reinstalled,
                },
                definition: Some(DefinitionKey {
                    role: DefinitionRole::Configured,
                    artifact: ArtifactRole::Target,
                }),
                sequence: 1,
                started_ms: 0,
                finished_ms: 1,
                complete: true,
            },
            entry_present: IntegrationProvenance::Observed {
                value: true,
                source: "original inspected entry".into(),
            },
            activated: unavailable(),
            primary_exited: unavailable(),
            descendant_scope_settled: unavailable(),
        };
        report.integrations[0].services.clear();
        report.integrations[0].checks.clear();
        report.integrations[0].desktops = vec![DesktopEvidence {
            slot: slot.clone(),
            observations: vec![observation],
            actions: vec![],
        }];
        let mut journal = NativeIntegrationJournal {
            kind: "theseus_native_journal".into(),
            schema: 2,
            profile: report.profile.clone(),
            run_id: report.run_id.clone(),
            recipe_digest: report.recipe_digest.clone(),
            core_recipe_digest: report.core_recipe_digest.clone(),
            core_manifest_digest: report.core_manifest_digest.clone(),
            integration_manifest_digest: report.integration_manifest_digest.clone(),
            manifest_digest: report.manifest_digest.clone(),
            requirements: report.requirements.clone(),
            compiled_requirements_digest: report.compiled_requirements_digest.clone(),
            selected_scenarios: report.selected_scenarios.clone(),
            launch_fingerprint: Some("13".repeat(32)),
            ownership: vec![NativeResource {
                scenario_id: "clean".into(),
                ownership_id: "16".repeat(16),
                catalog_authority: NativeCatalogAuthority::Valid,
                ownership: NativeOwnership::Acquired {
                    lease: operation.lease.clone(),
                    prepared: NativeIntent {
                        intent: "14".repeat(16),
                        binding: header.binding.clone(),
                        manifest_digest: report.manifest_digest.clone(),
                        workspace_marker: header.workspace_marker.clone(),
                        reset_epoch: "original-epoch".into(),
                        publication_scope: "file_and_directory_synced".into(),
                    },
                },
            }],
            operations: vec![operation],
            provider_calls: vec![],
            releases: vec![],
            recovery_attempts: vec![],
            failures: vec![],
            integration_ownership: vec![],
            integration_actions: vec![],
            broker_bindings: vec![],
            readiness_probes: vec![],
            integration_recoveries: vec![],
        };
        if activated {
            let binding = ResourceBindingSnapshot {
                digest: request.prior_binding_digest.clone(),
                basis: BindingBasis::AdmittedActivation,
                evidence_ids: vec!["original-observation".into()],
                definition: Some(DefinitionKey::from_desktop_wire(generation.definition)),
                artifact_commitment: "17".repeat(32),
                namespace_or_session_receipt_digest: header.digest().unwrap().to_hex(),
                original_action: Some(generation.sut.operation.clone()),
            };
            journal
                .integration_ownership
                .push(IntegrationResourceRecord {
                    key: IntegrationResourceKey {
                        scenario_id: "clean".into(),
                        ownership_id: "16".repeat(16),
                        slot: slot.clone(),
                    },
                    kind: IntegrationResourceKind::Desktop,
                    manifest_digest: report.manifest_digest.clone(),
                    initial_binding: binding.clone(),
                    binding_history: vec![binding.clone()],
                    current_binding_digest: binding.digest,
                    revocation: None,
                    disposition: Disposition::Unknown {
                        reason: "original local retirement not published".into(),
                    },
                });
            let wire = v2::Request::DesktopAction {
                request: Box::new(request.clone()),
            };
            journal.integration_actions.push(IntegrationActionRecord {
                ownership_id: "16".repeat(16),
                lease: request.helper_lease.clone(),
                slot: slot.clone(),
                operation: request.helper_operation.clone(),
                request_digest: v2::request_digest(&wire).unwrap().to_hex(),
                prior_binding_digest: request.prior_binding_digest.clone(),
                broker_identity: Some(
                    serde_json::from_value(
                        serde_json::to_value(request.identity().unwrap()).unwrap(),
                    )
                    .unwrap(),
                ),
                request: IntegrationRequest::Desktop {
                    lease: request.helper_lease.clone(),
                    operation: request.helper_operation.clone(),
                    slot: slot.clone(),
                    prior_binding_digest: request.prior_binding_digest.clone(),
                    mode: DesktopMode::OwnedExecutable,
                    request: Box::new(request),
                },
                completion: Disposition::Unknown {
                    reason: "original reply not cached".into(),
                },
                terminal: None,
            });
            journal.broker_bindings.push(BrokerBindingRecord {
                original: original.clone(),
                ownership_id: original.ownership_id.clone(),
                helper_lease: original.helper_lease.clone(),
                slot,
                original_intent: BrokerIntentRecord {
                    intent: original.broker_intent.clone(),
                    binding: header.binding.clone(),
                    manifest_digest: header.manifest_digest.clone(),
                    workspace_marker: header.workspace_marker.clone(),
                },
                publication_scope: Some(broker::IntentPublicationScope::FileAndDirectorySynced),
                original_intent_reference: format!("{}.json", original.broker_intent),
                expected_header_digest: header.digest().unwrap().to_hex(),
                session_id: header.session_id,
                session_epoch: header.session_epoch,
                broker_lease: Some("09".repeat(16)),
                disposition: Disposition::Complete {
                    receipt: "original durable broker mapping".into(),
                },
            });
        }
        validate_pair(&report, &journal).unwrap();
        (report, journal)
    }

    #[test]
    fn ordinary_saved_entry_generation_cannot_disappear() {
        for scenario in [ScenarioKind::Upgrade, ScenarioKind::Reinstall] {
            let (mut report, journal) = pair(scenario, false);
            report.integrations[0].desktops[0].observations[0].generation = None;
            assert!(
                report.validate().is_err(),
                "ordinary public entry lost its keyed generation"
            );
            assert!(validate_pair(&report, &journal).is_err());
        }
    }

    #[test]
    fn keyed_original_activation_requires_public_generation_even_with_legacy_binding() {
        let (mut report, journal) = pair(ScenarioKind::Clean, true);
        let observation = &mut report.integrations[0].desktops[0].observations[0];
        // Clearing the public key as well must not disguise the private keyed Activate.
        observation.binding.definition = None;
        observation.generation = None;
        report.validate().unwrap();
        assert!(
            validate_pair(&report, &journal).is_err(),
            "keyed original activation lost its public projection"
        );
    }

    #[test]
    fn keyed_original_activation_requires_exact_nonce_free_projection() {
        for scenario in [ScenarioKind::Upgrade, ScenarioKind::Reinstall] {
            let (mut report, journal) = pair(scenario, true);
            report.integrations[0].desktops[0].observations[0]
                .generation
                .as_mut()
                .unwrap()
                .tuple_digest = "15".repeat(32);
            assert!(
                validate_pair(&report, &journal).is_err(),
                "changed tuple borrowed original Activate"
            );
        }
    }

    #[test]
    fn baseline_uninstalled_and_legacy_clean_allow_absent_generation() {
        for (scenario, phase) in [
            (ScenarioKind::Upgrade, IntegrationPhase::Baseline),
            (ScenarioKind::Reinstall, IntegrationPhase::Uninstalled),
            (ScenarioKind::Clean, IntegrationPhase::Configured),
        ] {
            let (mut report, journal) = pair(scenario, false);
            let observation = &mut report.integrations[0].desktops[0].observations[0];
            observation.generation = None;
            observation.binding.definition = None;
            observation.binding.phase = phase;
            validate_pair(&report, &journal).unwrap();
        }
    }

    #[test]
    fn legacy_singleton_activate_keeps_absent_public_generation() {
        let (mut report, mut journal) = pair(ScenarioKind::Clean, true);
        let observation = &mut report.integrations[0].desktops[0].observations[0];
        observation.binding.definition = None;
        observation.generation = None;
        let action = &mut journal.integration_actions[0];
        let IntegrationRequest::Desktop { request, .. } = &mut action.request else {
            unreachable!()
        };
        if let broker::DesktopAction::Activate { transition, .. } = &mut request.action {
            *transition = None;
        }
        let close_digest = request.close_request().unwrap().digest().unwrap().to_hex();
        if let broker::DesktopAction::Activate { close_binding, .. } = &mut request.action {
            close_binding.action_digest = close_digest;
        }
        action.broker_identity = Some(
            serde_json::from_value(serde_json::to_value(request.identity().unwrap()).unwrap())
                .unwrap(),
        );
        action.request_digest = v2::request_digest(&action.request.wire().unwrap())
            .unwrap()
            .to_hex();
        validate_pair(&report, &journal).unwrap();
    }
}

#[cfg(test)]
mod owned_desktop_mode_tests {
    use super::*;
    use theseus_native_protocol::v2::broker::*;
    fn scenario() -> IntegrationScenarioEvidence {
        let mut scenario = crate::native::integration::view::tests::report()
            .integrations
            .remove(0);
        let definition = OwnedDesktopLifetimeDefinition::MacosOriginalPrimaryV1;
        let owned = OwnedLifetimeState {
            definition,
            primary: OwnedPrimaryState::Exited,
            descendants: OwnedTreeState::Unavailable,
            local_resources: OwnedLocalState::Retired,
        };
        let mut state = BrokerState {
            owned_lifetime: Some(OwnedLifetimeState::initial(definition)),
            owner: BrokerOwner::Retained,
            quarantined: false,
            ..BrokerState::unknown()
        };
        state.record_owned_lifetime(owned.clone()).unwrap();
        state.effect = BrokerEffect::Receipt {
            receipt: BrokerEffectReceipt {
                evidence: theseus_native_protocol::v2::broker::DesktopEvidence::OwnedExecutable,
                acknowledged: true,
                receipt_digest: "ab".repeat(32),
            },
        };
        state.validate().unwrap();
        assert!(!state.cleanup_complete);
        let mut bound = super::native_desktop_mode_tests::observation(state);
        bound.evidence =
            Some(theseus_native_protocol::v2::broker::DesktopEvidence::OwnedExecutable);
        let original = ActionReference {
            operation: bound.binding.action.clone().unwrap(),
            request_digest: "04".repeat(32),
        };
        scenario.desktops = vec![super::DesktopEvidence {
            slot: bound.binding.slot.clone(),
            actions: vec![original.clone()],
            observations: vec![bound],
        }];
        scenario.native_lifetime = None;
        scenario.owned_lifetime = Some(OwnedDesktopLifetimeProjection {
            original,
            state: owned,
        });
        let done = || Disposition::Complete {
            receipt: "separate original obligation completed".into(),
        };
        scenario.dispositions = IntegrationDispositions {
            helper_worker: done(),
            registration: done(),
            manager_action: done(),
            broker: done(),
            primary: done(),
            descendant_scope: Disposition::NotStarted,
        };
        scenario
    }
    #[test]
    fn explicit_original_owned_primary_can_complete_without_a_tree_claim() {
        let scenario = scenario();
        assert!(scenario.complete());
        assert!(
            !scenario.dispositions.complete(),
            "legacy strong completion is unchanged"
        );
        let state = scenario.desktops[0].observations[0]
            .broker_state
            .as_ref()
            .unwrap();
        assert!(!state.cleanup_complete);
        assert_eq!(state.descendants, BrokerDescendants::Unknown);
    }
    #[test]
    fn owned_primary_never_waives_missing_original_projection_or_uncertainty() {
        let original = scenario();
        let mut bad = original.clone();
        bad.owned_lifetime = None;
        assert!(!bad.complete());
        let mut bad = original.clone();
        bad.owned_lifetime.as_mut().unwrap().original.request_digest = "05".repeat(32);
        assert!(!bad.complete());
        let mut bad = original.clone();
        bad.dispositions.descendant_scope = Disposition::Unknown {
            reason: "original historical uncertainty".into(),
        };
        assert!(!bad.complete());
        let mut bad = original.clone();
        bad.dispositions.broker = Disposition::Unknown {
            reason: "original lease still held".into(),
        };
        assert!(!bad.complete());
        let mut bad = original.clone();
        bad.desktops[0].observations[0]
            .broker_state
            .as_mut()
            .unwrap()
            .quarantined = true;
        assert!(!bad.complete());
        let mut bad = original.clone();
        bad.owned_lifetime.as_mut().unwrap().state.descendants = OwnedTreeState::Unknown;
        assert!(!bad.complete());
        let mut erased_latest = original.clone();
        let mut last = erased_latest.desktops[0].observations[0].clone();
        last.broker_state.as_mut().unwrap().owned_lifetime = None;
        erased_latest.desktops[0].observations.push(last);
        assert!(
            !erased_latest.complete(),
            "an earlier retired sample cannot hide erased latest ownership facts"
        );
        let legacy = crate::native::integration::view::tests::report()
            .integrations
            .remove(0);
        assert!(
            serde_json::to_value(legacy)
                .unwrap()
                .get("owned_lifetime")
                .is_none()
        );
    }
    #[test]
    fn compact_owned_lifetime_keeps_unavailable_tree_and_local_retirement_visible() {
        let mut report = crate::native::integration::view::tests::report();
        report.integrations = vec![scenario()];
        let compact =
            serde_json::to_value(crate::native::integration::view::summary(&report)).unwrap();
        assert_eq!(
            compact["owned_lifetimes"][0]["state"]["descendants"],
            "unavailable"
        );
        assert_eq!(
            compact["owned_lifetimes"][0]["state"]["local_resources"],
            "retired"
        );
        assert_eq!(compact["owned_lifetimes"][0]["state"]["primary"], "exited");
        let legacy = crate::native::integration::view::tests::report();
        assert!(
            serde_json::to_value(crate::native::integration::view::summary(&legacy))
                .unwrap()
                .get("owned_lifetimes")
                .is_none()
        );
    }

    #[test]
    fn legacy_original_recovery_cannot_acquire_owned_primary_capability() {
        // Saved DTOs only; this does not construct a Host proof or run a target.
        let (_, journal) =
            super::desktop_generation_presence_tests::pair(ScenarioKind::Clean, true);
        let mut action = journal.integration_actions[0].clone();
        action.terminal = None;
        let state = scenario().desktops[0].observations[0]
            .broker_state
            .clone()
            .unwrap();
        assert!(
            validate_original_broker_effect(&action, &state).is_err(),
            "a legacy original Activate cannot acquire another lifetime contract during recovery"
        );
    }
    // Valid saved DTO pair only; no Host owner, application or native target is created.
    fn owned_saved_pair() -> (NativeIntegrationReport, NativeIntegrationJournal) {
        use theseus_native_protocol::v2;
        let (mut report, mut journal) =
            super::desktop_generation_presence_tests::pair(ScenarioKind::Clean, true);
        report.requirements.desktop.as_mut().unwrap().lifetime =
            LifetimeRequirement::OwnedPrimaryExited;
        report.compiled_requirements_digest = report.requirements.digest().unwrap();
        journal.requirements = report.requirements.clone();
        journal.compiled_requirements_digest = report.compiled_requirements_digest.clone();
        let mut header = journal.broker_bindings[0].original.expected_header.clone();
        header.os = "macos".into();
        header.display = SuppliedDisplay::MacOsGui {
            domain: "gui/1000".into(),
        };
        header.validate().unwrap();
        let header_digest = header.digest().unwrap().to_hex();
        let binding = &mut journal.broker_bindings[0];
        binding.original.expected_header = header;
        binding.expected_header_digest = header_digest.clone();
        journal.integration_ownership[0]
            .initial_binding
            .namespace_or_session_receipt_digest = header_digest.clone();
        journal.integration_ownership[0].binding_history[0].namespace_or_session_receipt_digest =
            header_digest.clone();
        let action = &mut journal.integration_actions[0];
        let IntegrationRequest::Desktop { request, .. } = &mut action.request else {
            panic!()
        };
        request.binding_digest = header_digest;
        if let DesktopAction::Activate { owned_lifetime, .. } = &mut request.action {
            *owned_lifetime = Some(OwnedDesktopLifetimeDefinition::MacosOriginalPrimaryV1);
        }
        let close_digest = request.close_request().unwrap().digest().unwrap().to_hex();
        if let DesktopAction::Activate { close_binding, .. } = &mut request.action {
            close_binding.action_digest = close_digest;
        }
        action.broker_identity = Some(
            serde_json::from_value(serde_json::to_value(request.identity().unwrap()).unwrap())
                .unwrap(),
        );
        let receipt_digest = request.digest().unwrap().to_hex();
        action.request_digest = v2::request_digest(&action.request.wire().unwrap())
            .unwrap()
            .to_hex();
        action.completion = Disposition::Complete {
            receipt: receipt_digest.clone(),
        };
        action.terminal = Some(IntegrationOutcome::Complete {
            receipt: receipt_digest.clone(),
        });
        let original = ActionReference {
            operation: action.operation.clone(),
            request_digest: action.request_digest.clone(),
        };
        let completed = scenario();
        let mut state = completed.desktops[0].observations[0]
            .broker_state
            .clone()
            .unwrap();
        let BrokerEffect::Receipt { receipt } = &mut state.effect else {
            panic!()
        };
        receipt.receipt_digest = receipt_digest;
        receipt.evidence =
            theseus_native_protocol::v2::broker::DesktopEvidence::SourceOwnedExecutable;
        let integration = &mut report.integrations[0];
        integration.dispositions = completed.dispositions;
        integration.owned_lifetime = Some(OwnedDesktopLifetimeProjection {
            original: original.clone(),
            state: state.owned_lifetime.clone().unwrap(),
        });
        integration.desktops[0].actions = vec![original];
        integration.desktops[0].observations[0].broker_state = Some(state);
        validate_pair(&report, &journal).expect("positive owned saved pair");
        (report, journal)
    }
    #[test]
    fn owned_saved_pair_binds_actual_private_original_and_retains_pending_history() {
        let (mut report, journal) = owned_saved_pair();
        assert!(report.integrations[0].complete());
        let mut pending = report.integrations[0].desktops[0].observations[0].clone();
        pending.binding.id = "earlier-owned-pending".into();
        pending.binding.sequence = 0;
        pending.binding.complete = false;
        pending.evidence = None;
        pending.broker_state = Some(BrokerState {
            owned_lifetime: Some(OwnedLifetimeState::initial(
                OwnedDesktopLifetimeDefinition::MacosOriginalPrimaryV1,
            )),
            native_lifetime: None,
            effect: BrokerEffect::Unknown,
            owner: BrokerOwner::Retained,
            primary: BrokerPrimary::Unknown,
            descendants: BrokerDescendants::Unknown,
            cleanup_complete: false,
            quarantined: false,
        });
        report.integrations[0].desktops[0]
            .observations
            .insert(0, pending);
        validate_pair(&report, &journal)
            .expect("a later original terminal cannot erase legitimate Pending history");
    }
    #[test]
    fn owned_saved_pair_rejects_two_public_digest_changes_without_private_change() {
        let (mut report, journal) = owned_saved_pair();
        let wrong = "de".repeat(32);
        report.integrations[0]
            .owned_lifetime
            .as_mut()
            .unwrap()
            .original
            .request_digest = wrong.clone();
        report.integrations[0].desktops[0].actions[0].request_digest = wrong;
        report.validate().unwrap();
        journal.validate().unwrap();
        assert!(
            validate_pair(&report, &journal).is_err(),
            "public-to-public equality is not exact private original request binding"
        );
    }
    #[test]
    fn owned_saved_pair_rejects_observed_receipt_for_another_original_effect() {
        let (mut report, journal) = owned_saved_pair();
        let state = report.integrations[0].desktops[0].observations[0]
            .broker_state
            .as_mut()
            .unwrap();
        let BrokerEffect::Receipt { receipt } = &mut state.effect else {
            panic!()
        };
        receipt.receipt_digest = "ed".repeat(32);
        report.validate().unwrap();
        journal.validate().unwrap();
        assert!(
            validate_pair(&report, &journal).is_err(),
            "owned report receipt must equal private original acknowledged terminal"
        );
    }
}
