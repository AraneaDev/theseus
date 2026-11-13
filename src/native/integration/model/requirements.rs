use super::serde_util::required_optional;
use super::*;
use crate::model::ScenarioKind;
use crate::native::model::*;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

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
