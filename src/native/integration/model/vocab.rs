use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

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
