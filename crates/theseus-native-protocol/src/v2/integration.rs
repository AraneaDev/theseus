//! Explicit integration profile DTOs. No native handles or implicit endpoints.
mod desktop_definitions;
mod linux_qualification;
mod native_desktop;
mod owned_entry;
mod platform;
pub use super::broker::OwnedDesktopLifetimeDefinition;
use super::{Digest, Error, NativeDescription};
pub use desktop_definitions::*;
pub use linux_qualification::*;
pub use native_desktop::*;
pub use owned_entry::*;
pub use platform::*;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceSlot(String);
impl ResourceSlot {
    pub fn new(value: &str) -> Result<Self, Error> {
        let b = value.as_bytes();
        if b.is_empty()
            || b.len() > 32
            || !b[0].is_ascii_lowercase()
            || !b
                .iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(c))
        {
            return Err(Error::InvalidValue);
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAction {
    Start,
    Stop,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceScope {
    User,
    System,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LocalEndpoint {
    UnixPath { path: String },
    NamedPipe { path: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagerQualification {
    pub manager: String,
    pub version: String,
    pub api_profile: String,
    pub features: Vec<String>,
    pub receipt_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RegistrationNamespace {
    ExclusiveOwnedRegistration {
        slot: ResourceSlot,
        unit_load_roots: Vec<String>,
        dropins: String,
        provider_context: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagerContext {
    WholePrivate {
        slot: ResourceSlot,
        sut_allowed_actions: Vec<String>,
        rehearsal_allowed_actions: Vec<String>,
        provider_receipt_required: bool,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceAuthority {
    Launchd {
        slot: ResourceSlot,
        domain: LaunchdDomain,
        label: String,
        definition_root: String,
        exclusive_load_roots: Vec<String>,
        launchctl: String,
        qualification: LaunchdQualification,
        managed_by: String,
        sut_effects: Vec<String>,
        allowed_actions: Vec<String>,
    },
    Scm {
        slot: ResourceSlot,
        context: ScmContext,
        service_name: String,
        qualification: ManagerQualification,
        allowed_access: u32,
        allow_local_system: bool,
        preflight: ScmPreflight,
        managed_by: String,
        sut_effects: Vec<String>,
        allowed_actions: Vec<String>,
    },
    Systemd {
        slot: ResourceSlot,
        scope: ServiceScope,
        bus_endpoint: LocalEndpoint,
        qualification: ManagerQualification,
        namespace: RegistrationNamespace,
        manager_context: ManagerContext,
        expected_peer_uid: u32,
        expected_manager_uid: u32,
        definition_root: String,
        unit: String,
        managed_by: String,
        sut_effects: Vec<String>,
        allowed_actions: Vec<String>,
    },
}
impl ServiceAuthority {
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Launchd { .. } => return self.validate_launchd(),
            Self::Scm { .. } => return self.validate_scm(),
            Self::Systemd { .. } => {}
        }
        let Self::Systemd {
            slot,
            bus_endpoint,
            qualification,
            namespace,
            manager_context,
            definition_root,
            unit,
            managed_by,
            sut_effects,
            allowed_actions,
            ..
        } = self
        else {
            unreachable!()
        };
        ResourceSlot::new(slot.as_str())?;
        let LocalEndpoint::UnixPath { path } = bus_endpoint else {
            return Err(Error::InvalidValue);
        };
        if !path.starts_with('/')
            || path.len() > 100
            || !path.is_ascii()
            || path.contains(['\0', ',', ';'])
        {
            return Err(Error::InvalidValue);
        }
        if !definition_root.starts_with('/')
            || definition_root.len() > 4096
            || definition_root.contains('\0')
            || unit.len() > 128
            || !unit.ends_with(".service")
            || unit.contains(['/', '\\', '@', '\0'])
        {
            return Err(Error::InvalidValue);
        }
        if qualification.manager != "systemd"
            || qualification.api_profile != "systemd_dbus_private_v1"
            || qualification.version.is_empty()
            || qualification.version.len() > 256
        {
            return Err(Error::InvalidValue);
        }
        Digest::parse(&qualification.receipt_sha256)?;
        if qualification.features != ["unit_properties", "start_stop", "manager_reload"] {
            return Err(Error::InvalidValue);
        }
        let RegistrationNamespace::ExclusiveOwnedRegistration {
            slot: ns_slot,
            unit_load_roots,
            dropins,
            provider_context,
        } = namespace;
        if slot != ns_slot
            || unit_load_roots.is_empty()
            || unit_load_roots.len() > 8
            || !unit_load_roots.contains(definition_root)
            || unit_load_roots
                .iter()
                .any(|p| !p.starts_with('/') || p.contains('\0') || p.len() > 4096)
            || dropins != "forbidden"
            || provider_context.is_empty()
        {
            return Err(Error::InvalidValue);
        }
        let ManagerContext::WholePrivate {
            sut_allowed_actions,
            rehearsal_allowed_actions,
            provider_receipt_required,
            ..
        } = manager_context;
        if *sut_allowed_actions != ["daemon_reload"]
            || !rehearsal_allowed_actions.is_empty()
            || !provider_receipt_required
            || managed_by != "installer"
            || *sut_effects != ["registration", "configuration", "removal", "daemon_reload"]
            || *allowed_actions != ["inspect", "start", "stop"]
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn slot(&self) -> &ResourceSlot {
        match self {
            Self::Systemd { slot, .. } | Self::Launchd { slot, .. } | Self::Scm { slot, .. } => {
                slot
            }
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationCapabilities {
    Service,
    Desktop,
    NativeDesktop,
    Combined,
    NativeCombined,
}
// Preserve the original public service-only value spelling and literal bytes.
#[allow(non_upper_case_globals)]
pub const IntegrationCapabilities: IntegrationCapabilities = IntegrationCapabilities::Service;
impl IntegrationCapabilities {
    pub const NAMES: [&'static str; 3] = ["inspect_service", "service_start", "service_stop"];
    pub const DESKTOP_NAMES: [&'static str; 3] =
        ["inspect_desktop", "owned_executable", "request_close"];
    pub fn names(&self) -> &'static [&'static str] {
        match self {
            Self::Service => &Self::NAMES,
            Self::Desktop => &Self::DESKTOP_NAMES,
            Self::NativeDesktop => &["inspect_desktop", "native_activation", "request_close"],
            Self::Combined => &[
                "inspect_service",
                "service_start",
                "service_stop",
                "inspect_desktop",
                "owned_executable",
                "request_close",
            ],
            Self::NativeCombined => &[
                "inspect_service",
                "service_start",
                "service_stop",
                "inspect_desktop",
                "native_activation",
                "request_close",
            ],
        }
    }
    pub fn for_manifest(manifest: &IntegrationManifest) -> Result<Self, Error> {
        match (
            manifest.service_authority.is_some(),
            manifest.desktop.is_some(),
        ) {
            (true, false) => Ok(Self::Service),
            (false, true) => Ok(if manifest.desktop_native.is_some() {
                Self::NativeDesktop
            } else {
                Self::Desktop
            }),
            (true, true) => Ok(if manifest.desktop_native.is_some() {
                Self::NativeCombined
            } else {
                Self::Combined
            }),
            _ => Err(Error::InvalidValue),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationDescription {
    pub namespace_receipt_digest: Digest,
    pub manifest: Box<IntegrationManifest>,
    pub context_digest: Digest,
    pub ownership_id: String,
    pub native: NativeDescription,
    pub core_manifest_digest: Digest,
    pub integration_manifest_digest: Digest,
    pub capabilities: IntegrationCapabilities,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceObservation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<ServicePlatformFacts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    pub slot: ResourceSlot,
    pub registration_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition_digest: Option<String>,
    pub configured: bool,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub manager_generation: String,
    pub complete: bool,
    pub binding_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceOutcome {
    Complete { receipt: String },
    Unknown { reason: String },
    Failed { reason: String },
}

/// Provider-reported per-instance context, independently bound to immutable fullM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RuntimeContextInput")]
pub struct IntegrationRuntimeContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linux_gio_qualification: Option<LinuxGioQualificationRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installation_instance_digest: Option<String>,
    #[serde(skip_serializing_if = "absent_or_systemd")]
    pub manager_platform: Option<ManagerPlatform>,
    pub kind: String,
    pub schema: u32,
    pub ownership_id: String,
    pub binding: String,
    pub manifest_digest: String,
    pub workspace_marker: String,
    pub provider_epoch: String,
    pub integration_manifest_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manager_unique_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace_receipt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop_broker_header: Option<super::broker::IntegrationBrokerHeader>,
}
fn absent_or_systemd(value: &Option<ManagerPlatform>) -> bool {
    value.is_none() || *value == Some(ManagerPlatform::Systemd)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeContextInput {
    linux_gio_qualification: Option<LinuxGioQualificationRef>,
    installation_instance_digest: Option<String>,
    manager_platform: Option<ManagerPlatform>,
    kind: String,
    schema: u32,
    ownership_id: String,
    binding: String,
    manifest_digest: String,
    workspace_marker: String,
    provider_epoch: String,
    integration_manifest_digest: String,
    manager_unique_name: Option<String>,
    namespace_receipt: Option<String>,
    desktop_broker_header: Option<super::broker::IntegrationBrokerHeader>,
}
impl TryFrom<RuntimeContextInput> for IntegrationRuntimeContext {
    type Error = Error;
    fn try_from(raw: RuntimeContextInput) -> Result<Self, Error> {
        // Old systemd contexts omit their default platform. A desktop context
        // with no manager material must not synthesize that manager authority.
        let manager_platform = raw.manager_platform.or_else(|| {
            (raw.manager_unique_name.is_some() && raw.namespace_receipt.is_some())
                .then_some(ManagerPlatform::Systemd)
        });
        let value = Self {
            linux_gio_qualification: raw.linux_gio_qualification,
            installation_instance_digest: raw.installation_instance_digest,
            manager_platform,
            kind: raw.kind,
            schema: raw.schema,
            ownership_id: raw.ownership_id,
            binding: raw.binding,
            manifest_digest: raw.manifest_digest,
            workspace_marker: raw.workspace_marker,
            provider_epoch: raw.provider_epoch,
            integration_manifest_digest: raw.integration_manifest_digest,
            manager_unique_name: raw.manager_unique_name,
            namespace_receipt: raw.namespace_receipt,
            desktop_broker_header: raw.desktop_broker_header,
        };
        value.validate()?;
        Ok(value)
    }
}
impl IntegrationRuntimeContext {
    pub fn validate(&self) -> Result<(), Error> {
        if self.kind != "theseus_native_integration_context"
            || self.schema != 1
            || self.ownership_id.is_empty()
            || self.ownership_id.len() > 128
        {
            return Err(Error::InvalidValue);
        }
        match (
            &self.manager_platform,
            &self.manager_unique_name,
            &self.namespace_receipt,
        ) {
            (Some(platform), Some(name), Some(receipt)) => {
                if receipt.is_empty()
                    || receipt.len() > 4096
                    || receipt.contains('\0')
                    || name.is_empty()
                    || name.len() > 128
                    || name.contains('\0')
                    || (*platform == ManagerPlatform::Systemd) != name.starts_with(':')
                    || (*platform == ManagerPlatform::Systemd)
                        != self.installation_instance_digest.is_none()
                {
                    return Err(Error::InvalidValue);
                }
            }
            (None, None, None)
                if self.desktop_broker_header.is_some()
                    && self.installation_instance_digest.is_none() => {}
            _ => return Err(Error::InvalidValue),
        }
        if let Some(digest) = &self.installation_instance_digest {
            Digest::parse(digest)?;
        }
        super::BindingId::parse(&self.binding)?;
        super::WorkspaceMarker::parse(&self.workspace_marker)?;
        for value in [
            &self.manifest_digest,
            &self.provider_epoch,
            &self.integration_manifest_digest,
        ] {
            Digest::parse(value)?;
        }
        if let Some(reference) = &self.linux_gio_qualification {
            reference.validate()?;
        }
        if self.linux_gio_qualification.as_ref()
            != self
                .desktop_broker_header
                .as_ref()
                .and_then(|header| header.linux_gio_qualification.as_ref())
        {
            return Err(Error::InvalidValue);
        }
        if let Some(header) = &self.desktop_broker_header {
            header.validate()?;
            if header.binding != self.binding
                || header.manifest_digest != self.manifest_digest
                || header.workspace_marker != self.workspace_marker
                || header.ownership_id != self.ownership_id
                || header.provider_epoch != self.provider_epoch
            {
                return Err(Error::InvalidValue);
            }
        }
        Ok(())
    }
    /// Digest of the actual supplied namespace or original desktop session receipt.
    pub fn authority_receipt_digest(&self) -> Result<Digest, Error> {
        use sha2::{Digest as _, Sha256};
        self.validate()?;
        if let Some(receipt) = &self.namespace_receipt {
            Ok(Digest::from_bytes(
                Sha256::digest(receipt.as_bytes()).into(),
            ))
        } else {
            self.desktop_broker_header
                .as_ref()
                .ok_or(Error::InvalidValue)?
                .digest()
        }
    }
    pub fn digest(&self) -> Result<Digest, Error> {
        use sha2::{Digest as _, Sha256};
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| Error::InvalidValue)?;
        let mut hash = Sha256::new();
        hash.update(b"theseus.native_integration_v1.context\0");
        hash.update(bytes);
        Ok(Digest::from_bytes(hash.finalize().into()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scm: Option<ScmDefinition>,
    pub role: String,
    pub artifact: String,
    pub executable: String,
    pub arguments: Vec<String>,
    pub environment: Vec<String>,
    pub working_directory: String,
    pub definition_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationArtifactSlot {
    pub slot: ResourceSlot,
    pub relative_path: String,
    pub length: u64,
    pub sha256: String,
}
/// Independently committed owned launch authority. Entry bytes cannot supply or
/// change any command/environment/path value in this manifest commitment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopExecutable {
    pub root: String,
    pub relative_path: String,
    pub length: u64,
    pub sha256: String,
    pub arguments: Vec<String>,
    /// OwnedExecutable: exact application replacement environment. NativeActivation:
    /// exact broker startup replacement ceiling only; no application environment promise.
    pub environment: Vec<DesktopEnvironment>,
    /// Exact owned application cwd. Native Linux/macOS: broker startup cwd only;
    /// native Windows additionally binds the exact shortcut application cwd/root.
    /// Entry bytes and the complete literal argv remain pinned in every mode.
    pub working_directory: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopEnvironment {
    pub name: String,
    pub value: String,
}
impl DesktopExecutable {
    pub fn validate(&self, desktop: &super::broker::DesktopManifest) -> Result<(), Error> {
        let platform = match &desktop.ready_endpoint {
            LocalEndpoint::UnixPath { .. } => ManagerPlatform::Systemd,
            LocalEndpoint::NamedPipe { .. } => ManagerPlatform::Scm,
        };
        let parts = self.relative_path.split('/').collect::<Vec<_>>();
        let slash_executable = format!("{}/{}", self.root, self.relative_path);
        let native_windows_executable =
            format!("{}\\{}", self.root, self.relative_path.replace('/', "\\"));
        let argv_matches = self.arguments.first().is_some_and(|program| {
            program == &slash_executable
                || (platform == ManagerPlatform::Scm && program == &native_windows_executable)
        });
        if self.length == 0
            || self.length > super::MAX_NATIVE_FILE_BYTES
            || self.sha256 != desktop.executable_digest
            || Digest::parse(&self.sha256).is_err()
            || !platform_absolute(&self.root, platform)
            || self.root.ends_with(['/', '\\'])
            || !platform_absolute(&self.working_directory, platform)
            || parts.is_empty()
            || parts.len() > 8
            || self.relative_path.len() > 4096
            || parts.iter().any(|p| super::LeafName::new(p).is_err())
            || self.arguments.is_empty()
            || self.arguments.len() > 16
            || !argv_matches
            || self
                .arguments
                .iter()
                .any(|v| v.len() > 512 || v.contains('\0'))
            || self.environment.len() > 32
        {
            return Err(Error::InvalidValue);
        }
        let mut previous: Option<&str> = None;
        let mut names = std::collections::BTreeSet::new();
        let mut total = self.arguments.iter().map(String::len).sum::<usize>();
        for e in &self.environment {
            let name = e.name.as_bytes();
            if name.is_empty()
                || name.len() > 128
                || !(name[0].is_ascii_alphabetic() || name[0] == b'_')
                || !name.iter().all(|v| v.is_ascii_alphanumeric() || *v == b'_')
                || e.value.len() > 1024
                || e.value.contains('\0')
                || previous.is_some_and(|p| p >= e.name.as_str())
                || !names.insert(e.name.to_ascii_uppercase())
            {
                return Err(Error::InvalidValue);
            }
            previous = Some(&e.name);
            total += e.name.len() + e.value.len();
        }
        if total > 16384 {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationManifest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness: Option<IntegrationReadiness>,
    pub schema: u32,
    pub core_manifest_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_authority: Option<ServiceAuthority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop: Option<super::broker::DesktopManifest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_executable: Option<DesktopExecutable>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_readiness: Option<ReadinessDefinitionExpectation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_native: Option<NativeDesktopDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_owned_lifetime: Option<OwnedDesktopLifetimeDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_owned_entry: Option<OwnedDesktopEntryDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub desktop_definitions: Vec<DesktopDefinition>,
    pub definitions: Vec<ServiceDefinition>,
    pub artifact_slots: Vec<IntegrationArtifactSlot>,
    pub readiness_protocol: String,
    pub desktop_capabilities: Vec<String>,
}
impl IntegrationManifest {
    pub fn validate(&self) -> Result<(), Error> {
        if self.schema != 1
            || self.artifact_slots.len() > 8
            || self.readiness_protocol != "theseus_fixture_ready_v1"
        {
            return Err(Error::InvalidValue);
        }
        Digest::parse(&self.core_manifest_digest)?;
        self.validate_desktop_definitions()?;
        if let Some(owned) = self.desktop_owned_lifetime {
            owned.validate_mode(
                &self
                    .desktop
                    .as_ref()
                    .ok_or(Error::InvalidValue)?
                    .allowed_modes,
            )?;
            if self.desktop_native.is_some()
                || self
                    .service_authority
                    .as_ref()
                    .is_some_and(|a| a.platform() != ManagerPlatform::Launchd)
            {
                return Err(Error::InvalidValue);
            }
        }
        if let Some(entry) = &self.desktop_owned_entry {
            if self.desktop_native.is_some()
                || self.desktop_owned_lifetime.is_some() && entry.os() != "macos"
                || self.service_authority.as_ref().is_some_and(|a| {
                    !matches!(
                        (a.platform(), entry.os()),
                        (ManagerPlatform::Launchd, "macos") | (ManagerPlatform::Scm, "windows")
                    )
                })
            {
                return Err(Error::InvalidValue);
            }
            let tuple = self.desktop_definition(&DesktopDefinitionKey::CONFIGURED_TARGET)?;
            entry.validate(&tuple.desktop, &tuple.executable, tuple.entry_length)?;
        }
        match &self.desktop {
            Some(desktop) => {
                desktop.validate()?;
                let executable = self
                    .desktop_executable
                    .as_ref()
                    .ok_or(Error::InvalidValue)?;
                executable.validate(desktop)?;
                if !self.artifact_slots.iter().any(|slot| {
                    slot.slot == desktop.slot
                        && slot.relative_path
                            == self.native_entry_relative_path().unwrap_or_default()
                        && slot.sha256 == desktop.entry_digest
                        && slot.length > 0
                        && slot.length <= 65536
                }) {
                    return Err(Error::InvalidValue);
                }
                let readiness = self.desktop_readiness.as_ref().ok_or(Error::InvalidValue)?;
                readiness.validate()?;
                if readiness.digest()?.to_hex() != desktop.expectation_digest
                    || readiness.artifact_sha256 != executable.sha256
                    || readiness.artifact_length != executable.length
                {
                    return Err(Error::InvalidValue);
                }
                let capabilities = match &self.desktop_native {
                    Some(native) => {
                        native.validate(desktop, executable)?;
                        if let Some(service) = &self.service_authority {
                            let same_platform = matches!(
                                (service.platform(), native.os()),
                                (ManagerPlatform::Systemd, "linux")
                                    | (ManagerPlatform::Launchd, "macos")
                                    | (ManagerPlatform::Scm, "windows")
                            );
                            if !same_platform || service.slot() == &desktop.slot {
                                return Err(Error::InvalidValue);
                            }
                        }
                        if let NativeDesktopDefinition::MacosBundle { info_plist, .. } = native
                            && !self.artifact_slots.iter().any(|slot| {
                                slot.slot == desktop.slot
                                    && slot.length == info_plist.length
                                    && slot.sha256 == info_plist.sha256
                            })
                        {
                            return Err(Error::InvalidValue);
                        }
                        ["inspect_desktop", "native_activation", "request_close"]
                    }
                    None => {
                        if desktop.allowed_modes != [super::broker::DesktopMode::OwnedExecutable] {
                            return Err(Error::InvalidValue);
                        }
                        ["inspect_desktop", "owned_executable", "request_close"]
                    }
                };
                if desktop.core_manifest_digest != self.core_manifest_digest
                    || self
                        .desktop_capabilities
                        .iter()
                        .map(String::as_str)
                        .ne(capabilities)
                {
                    return Err(Error::InvalidValue);
                }
            }
            None if !self.desktop_capabilities.is_empty()
                || self.desktop_executable.is_some()
                || self.desktop_readiness.is_some()
                || self.desktop_native.is_some()
                || self.desktop_owned_entry.is_some() =>
            {
                return Err(Error::InvalidValue);
            }
            None => {}
        }
        if let Some(authority) = &self.service_authority {
            if self.definitions.is_empty()
                || self.definitions.len() > 6
                || self.artifact_slots.len() > 8
                || self.readiness_protocol != "theseus_fixture_ready_v1"
                || (authority.platform() != ManagerPlatform::Systemd && self.readiness.is_none())
            {
                return Err(Error::InvalidValue);
            }
            if matches!(authority,ServiceAuthority::Launchd{qualification,..} if qualification.absence_profile.is_none())
            {
                return Err(Error::InvalidValue);
            }
            if let Some(readiness) = &self.readiness {
                if !readiness.valid_endpoint(authority.platform())
                    || readiness.expectations.is_empty()
                    || readiness.expectations.len() > 6
                {
                    return Err(Error::InvalidValue);
                }
                match authority {
                    ServiceAuthority::Launchd {
                        domain: LaunchdDomain::User { uid },
                        ..
                    } if readiness.peer_uid != Some(*uid) => return Err(Error::InvalidValue),
                    ServiceAuthority::Scm { preflight, .. } if !matches!(&readiness.endpoint,LocalEndpoint::NamedPipe{path} if path==&preflight.pipe) =>
                    {
                        return Err(Error::InvalidValue);
                    }
                    _ => {}
                }
                let mut keys = std::collections::BTreeSet::new();
                for e in &readiness.expectations {
                    e.validate()?;
                    if !keys.insert((&e.role, &e.artifact)) {
                        return Err(Error::InvalidValue);
                    }
                }
            }
            Digest::parse(&self.core_manifest_digest)?;
            authority.validate()?;
            let mut keys = std::collections::BTreeSet::new();
            for d in &self.definitions {
                if !["base", "configured", "target"].contains(&d.role.as_str())
                    || !["base", "target"].contains(&d.artifact.as_str())
                    || !keys.insert((&d.role, &d.artifact))
                    || !platform_absolute(&d.executable, authority.platform())
                    || !platform_absolute(&d.working_directory, authority.platform())
                    || d.arguments.is_empty()
                    || d.arguments.len() > 16
                    || d.arguments[0] != d.executable
                    || d.arguments
                        .iter()
                        .chain(&d.environment)
                        .any(|s| s.len() > 4096 || s.contains('\0'))
                    || d.environment.len() > 32
                {
                    return Err(Error::InvalidValue);
                }
                Digest::parse(&d.definition_sha256)?;
                if authority.platform() == ManagerPlatform::Scm {
                    let raw = d.scm.as_ref().ok_or(Error::InvalidValue)?;
                    raw.validate(d, authority)?;
                    if raw.digest()?.to_hex() != d.definition_sha256 {
                        return Err(Error::InvalidValue);
                    }
                } else if d.scm.is_some()
                    || (authority.platform() == ManagerPlatform::Launchd
                        && !d.environment.is_empty())
                {
                    return Err(Error::InvalidValue);
                }
            }
        } else if self.desktop.is_none() || !self.definitions.is_empty() || self.readiness.is_some()
        {
            return Err(Error::InvalidValue);
        }
        let mut slots = std::collections::BTreeSet::new();
        for a in &self.artifact_slots {
            ResourceSlot::new(a.slot.as_str())?;
            Digest::parse(&a.sha256)?;
            if !slots.insert(a.slot.as_str())
                || a.length == 0
                || a.length > super::MAX_NATIVE_FILE_BYTES
                || a.relative_path.len() > 4096
                || a.relative_path.split('/').count() > 8
                || a.relative_path
                    .split('/')
                    .any(|part| super::LeafName::new(part).is_err())
            {
                return Err(Error::InvalidValue);
            }
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<Digest, Error> {
        use sha2::{Digest as _, Sha256};
        self.validate()?;
        let mut h = Sha256::new();
        h.update(b"theseus.native_integration_v1.integration\0");
        h.update(serde_json::to_vec(self).map_err(|_| Error::InvalidValue)?);
        Ok(Digest::from_bytes(h.finalize().into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::*;
    #[test]
    fn catalog_ledger_cannot_accept_service_action_authority() {
        use ledger::{Entry, LedgerView, Record};
        let lease = LeaseId::from_bytes([1; 16]);
        let operation = OperationId::from_bytes([2; 16]);
        let records = vec![
            Record::new(
                0,
                Entry::NativeHeader {
                    binding: BindingId::from_bytes([3; 16]),
                    manifest_digest: Digest::from_bytes([4; 32]),
                    workspace_marker: WorkspaceMarker::from_bytes([5; 16]),
                    os: Os::Linux,
                    capabilities: NativeCapabilities,
                    provider_epoch: Digest::from_bytes([6; 32]),
                },
            )
            .unwrap(),
            Record::new(
                1,
                Entry::LeaseReserved {
                    intent: IntentId::from_bytes([7; 16]),
                    lease,
                },
            )
            .unwrap(),
            Record::new(
                2,
                Entry::OperationAccepted {
                    lease,
                    operation,
                    request_digest: Digest::from_bytes([8; 32]),
                    operation_kind: OperationKind::ServiceAction,
                },
            )
            .unwrap(),
        ];
        assert!(
            LedgerView::replay(&records).is_err(),
            "catalog profile must never acquire service authority"
        );
    }
    #[test]
    fn absent_service_observation_roundtrips_through_strict_v2_decoder() {
        let frame = Envelope::new(
            SessionId::from_bytes([5; 16]),
            2,
            Message::Response {
                reply_to: 1,
                result: Reply::ServiceObservation {
                    lease: LeaseId::from_bytes([1; 16]),
                    observation: Box::new(ServiceObservation {
                        platform: None,
                        instance: None,
                        slot: ResourceSlot::new("fixture_service").unwrap(),
                        registration_present: false,
                        definition_digest: None,
                        configured: false,
                        running: false,
                        origin: None,
                        manager_generation: ":1.0".into(),
                        complete: true,
                        binding_digest: "22".repeat(32),
                    }),
                },
            },
        )
        .unwrap();
        let bytes = encode(&frame).unwrap();
        let mut decoder = Decoder::new();
        assert_eq!(decoder.feed(&bytes).unwrap().frame.unwrap(), frame);
    }
    #[test]
    fn service_action_digest_binds_slot_control_and_original_prior() {
        let request = Request::ServiceAction {
            lease: LeaseId::from_bytes([1; 16]),
            operation: OperationId::from_bytes([2; 16]),
            slot: ResourceSlot::new("fixture_service").unwrap(),
            prior_binding_digest: Digest::from_bytes([3; 32]),
            action: ServiceAction::Start,
        };
        let digest = request_digest(&request).unwrap();
        let mut stop = request.clone();
        if let Request::ServiceAction { action, .. } = &mut stop {
            *action = ServiceAction::Stop;
        }
        assert_ne!(digest, request_digest(&stop).unwrap());
        let mut changed = request.clone();
        if let Request::ServiceAction {
            prior_binding_digest,
            ..
        } = &mut changed
        {
            *prior_binding_digest = Digest::from_bytes([4; 32]);
        }
        assert_ne!(digest, request_digest(&changed).unwrap());
        let frame = Envelope::new(
            SessionId::from_bytes([5; 16]),
            1,
            Message::Request {
                request: request.clone(),
            },
        )
        .unwrap();
        let bytes = encode(&frame).unwrap();
        let mut decoder = Decoder::new();
        assert_eq!(decoder.feed(&bytes).unwrap().frame.unwrap(), frame);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessDefinitionExpectation {
    pub role: String,
    pub artifact: String,
    pub artifact_length: u64,
    pub artifact_sha256: String,
    pub version_literal: String,
    pub data_sha256: String,
    pub configuration_digest: String,
}
impl ReadinessDefinitionExpectation {
    pub fn validate(&self) -> Result<(), Error> {
        if !["base", "configured", "target"].contains(&self.role.as_str())
            || !["base", "target"].contains(&self.artifact.as_str())
            || self.artifact_length == 0
            || self.artifact_length > super::MAX_NATIVE_FILE_BYTES
            || self.version_literal.is_empty()
            || self.version_literal.len() > 128
            || self.version_literal.contains('\0')
        {
            return Err(Error::InvalidValue);
        }
        for d in [
            &self.artifact_sha256,
            &self.data_sha256,
            &self.configuration_digest,
        ] {
            Digest::parse(d)?;
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<Digest, Error> {
        use sha2::{Digest as _, Sha256};
        self.validate()?;
        let mut h = Sha256::new();
        h.update(b"theseus.native_integration_v1.readiness_expectation\0");
        h.update(serde_json::to_vec(self).map_err(|_| Error::InvalidValue)?);
        Ok(Digest::from_bytes(h.finalize().into()))
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationReadiness {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooperative_instance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_sid: Option<String>,
    pub endpoint: LocalEndpoint,
    pub root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_gid: Option<u32>,
    pub expectations: Vec<ReadinessDefinitionExpectation>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessState {
    Ready,
    Failed,
    Incomplete,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessObservation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_sid: Option<String>,
    pub expectation: ReadinessDefinitionExpectation,
    pub slot: ResourceSlot,
    pub operation: String,
    pub action_digest: String,
    pub resource_binding_digest: String,
    pub challenge_digest: String,
    pub expectation_digest: String,
    pub state: ReadinessState,
    pub authenticated_instance: bool,
    pub instance_binding_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_uid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_gid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_literal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initialized: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessChallengeFields {
    pub lease: String,
    pub slot: String,
    pub resource_binding_digest: String,
    pub operation: String,
    pub action_digest: String,
    pub nonce: String,
    pub expectation_digest: String,
}
impl ReadinessChallengeFields {
    pub fn digest(&self) -> Result<Digest, Error> {
        use sha2::{Digest as _, Sha256};
        super::LeaseId::parse(&self.lease)?;
        ResourceSlot::new(&self.slot)?;
        super::OperationId::parse(&self.operation)?;
        super::BindingId::parse(&self.nonce)?;
        for d in [
            &self.resource_binding_digest,
            &self.action_digest,
            &self.expectation_digest,
        ] {
            Digest::parse(d)?;
        }
        let fields = [
            "theseus_fixture_ready_v1",
            "challenge",
            &self.lease,
            &self.slot,
            &self.resource_binding_digest,
            &self.operation,
            &self.action_digest,
            &self.nonce,
            &self.expectation_digest,
        ];
        let mut bytes = Vec::new();
        for field in fields {
            bytes.extend_from_slice(field.len().to_string().as_bytes());
            bytes.push(b':');
            bytes.extend_from_slice(field.as_bytes());
            bytes.push(b',');
        }
        Ok(Digest::from_bytes(Sha256::digest(bytes).into()))
    }
}

impl ReadinessObservation {
    pub fn validate(&self) -> Result<(), Error> {
        self.expectation.validate()?;
        ResourceSlot::new(self.slot.as_str())?;
        super::OperationId::parse(&self.operation)?;
        for d in [
            &self.action_digest,
            &self.resource_binding_digest,
            &self.challenge_digest,
            &self.expectation_digest,
            &self.instance_binding_digest,
        ] {
            Digest::parse(d)?;
        }
        if self.expectation.digest()?.to_hex() != self.expectation_digest {
            return Err(Error::InvalidValue);
        }
        for d in [
            &self.artifact_sha256,
            &self.data_sha256,
            &self.configuration_digest,
            &self.response_digest,
        ]
        .into_iter()
        .flatten()
        {
            Digest::parse(d)?;
        }
        if self
            .failure
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 4096 || s.contains('\0'))
            || self
                .version_literal
                .as_ref()
                .is_some_and(|s| s.len() > 128 || s.contains('\0'))
        {
            return Err(Error::InvalidValue);
        }
        if self.state == ReadinessState::Ready
            && (!self.authenticated_instance
                || !((self.peer_sid.is_none()
                    && self.peer_uid.is_some()
                    && self.peer_gid.is_some())
                    || (self.peer_sid.as_deref() == Some("S-1-5-18")
                        && self.peer_uid.is_none()
                        && self.peer_gid.is_none()))
                || self.initialized != Some(true)
                || self.response_digest.is_none()
                || self.instance_binding_digest != self.resource_binding_digest
                || self.artifact_sha256.as_ref() != Some(&self.expectation.artifact_sha256)
                || self.data_sha256.as_ref() != Some(&self.expectation.data_sha256)
                || self.configuration_digest.as_ref()
                    != Some(&self.expectation.configuration_digest)
                || self.version_literal.as_ref() != Some(&self.expectation.version_literal)
                || self.failure.is_some())
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    /// Desktop structural validation only. Original identity and trusted peer
    /// ceilings must be supplied independently through the contextual methods.
    pub fn validate_desktop_fields(&self) -> Result<(), Error> {
        self.expectation.validate()?;
        ResourceSlot::new(self.slot.as_str())?;
        super::OperationId::parse(&self.operation)?;
        for d in [
            &self.action_digest,
            &self.resource_binding_digest,
            &self.challenge_digest,
            &self.expectation_digest,
            &self.instance_binding_digest,
        ] {
            Digest::parse(d)?;
        }
        if self.expectation.digest()?.to_hex() != self.expectation_digest {
            return Err(Error::InvalidValue);
        }
        for d in [
            &self.artifact_sha256,
            &self.data_sha256,
            &self.configuration_digest,
            &self.response_digest,
        ]
        .into_iter()
        .flatten()
        {
            Digest::parse(d)?;
        }
        if self
            .failure
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 4096 || s.contains('\0'))
            || self
                .version_literal
                .as_ref()
                .is_some_and(|s| s.len() > 128 || s.contains('\0'))
        {
            return Err(Error::InvalidValue);
        }
        if self.state == ReadinessState::Ready
            && (!self.authenticated_instance
                || !((self.peer_sid.is_none()
                    && self.peer_uid.is_some()
                    && self.peer_gid.is_some())
                    || (self.peer_sid.as_ref().is_some_and(|sid| {
                        sid.starts_with("S-1-") && sid.len() <= 184 && !sid.contains('\0')
                    }) && self.peer_uid.is_none()
                        && self.peer_gid.is_none()))
                || self.initialized != Some(true)
                || self.response_digest.is_none()
                || self.instance_binding_digest != self.action_digest
                || self.artifact_sha256.as_ref() != Some(&self.expectation.artifact_sha256)
                || self.data_sha256.as_ref() != Some(&self.expectation.data_sha256)
                || self.configuration_digest.as_ref()
                    != Some(&self.expectation.configuration_digest)
                || self.version_literal.as_ref() != Some(&self.expectation.version_literal)
                || self.failure.is_some())
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn validate_desktop(
        &self,
        identity: &super::broker::BrokerOperationIdentity,
    ) -> Result<(), Error> {
        self.validate_desktop_fields()?;
        identity.validate()?;
        if self.operation != identity.helper_operation
            || self.action_digest != identity.request_digest
            || self.instance_binding_digest != identity.request_digest
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn validate_desktop_peer(
        &self,
        principal: &super::broker::BrokerPrincipal,
    ) -> Result<(), Error> {
        self.validate_desktop_fields()?;
        if self.state != ReadinessState::Ready {
            return Ok(());
        }
        let matches = match principal {
            super::broker::BrokerPrincipal::Posix {
                effective_uid,
                effective_gid,
            } => {
                self.peer_sid.is_none()
                    && self.peer_uid == Some(*effective_uid)
                    && self.peer_gid == Some(*effective_gid)
            }
            super::broker::BrokerPrincipal::Windows { token_user_sid } => {
                self.peer_sid.as_ref() == Some(token_user_sid)
                    && self.peer_uid.is_none()
                    && self.peer_gid.is_none()
            }
        };
        if !matches {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
}

#[cfg(test)]
mod unavailable_readiness_wire_tests {
    use super::*;
    use crate::v2::*;
    #[test]
    fn failed_and_incomplete_readiness_without_response_roundtrip_strict_wire() {
        for (state, failure) in [
            (ReadinessState::Incomplete, "Deadline"),
            (ReadinessState::Failed, "WrongPeer"),
        ] {
            let expectation = ReadinessDefinitionExpectation {
                role: "configured".into(),
                artifact: "target".into(),
                artifact_length: 1,
                artifact_sha256: "11".repeat(32),
                version_literal: "target".into(),
                data_sha256: "22".repeat(32),
                configuration_digest: "33".repeat(32),
            };
            let expectation_digest = expectation.digest().unwrap().to_hex();
            let observation = ReadinessObservation {
                expectation,
                slot: ResourceSlot::new("service").unwrap(),
                operation: "44".repeat(16),
                action_digest: "55".repeat(32),
                resource_binding_digest: "66".repeat(32),
                challenge_digest: "77".repeat(32),
                expectation_digest,
                state,
                authenticated_instance: false,
                instance_binding_digest: "66".repeat(32),
                peer_sid: None,
                peer_uid: None,
                peer_gid: None,
                artifact_sha256: None,
                version_literal: None,
                data_sha256: None,
                configuration_digest: None,
                initialized: None,
                response_digest: None,
                failure: Some(failure.into()),
            };
            observation.validate().unwrap();
            let frame = Envelope::new(
                SessionId::from_bytes([1; 16]),
                1,
                Message::Response {
                    reply_to: 0,
                    result: Reply::ReadinessObservation {
                        lease: LeaseId::from_bytes([2; 16]),
                        observation: Box::new(observation),
                    },
                },
            )
            .unwrap();
            let bytes = encode(&frame).unwrap();
            let mut decoder = Decoder::new();
            let decoded = decoder
                .feed(&bytes)
                .expect("unavailable application fields must remain absent on the strict wire");
            assert_eq!(decoded.frame, Some(frame));
        }
    }
}

#[cfg(test)]
mod platform_admission_tests {
    use super::*;
    fn launchd() -> serde_json::Value {
        serde_json::json!({"kind":"launchd","slot":"fixture_service","domain":{"kind":"user","uid":501},"label":"org.theseus.fixture","definition_root":"/fixture/plists","exclusive_load_roots":["/fixture/plists"],"launchctl":"/bin/launchctl","qualification":{"os_version":"15.0","os_build":"24A","api_profile":"launchctl-qualified-v1","receipt_sha256":"11".repeat(32),"launchctl_fingerprint":([1].into_iter().chain([0;56]).collect::<Vec<_>>())},"managed_by":"installer","sut_effects":["registration","configuration","removal"],"allowed_actions":["inspect","start","stop"]})
    }
    #[test]
    fn launchd_admits_only_exact_user_domain_and_qualified_executable() {
        let value = launchd();
        let authority: ServiceAuthority = serde_json::from_value(value.clone()).unwrap();
        authority.validate().unwrap();
        for (field, replacement) in [
            ("domain", serde_json::json!({"kind":"gui","uid":501})),
            ("launchctl", serde_json::json!("/usr/bin/launchctl")),
            ("exclusive_load_roots", serde_json::json!([])),
        ] {
            let mut bad = value.clone();
            bad[field] = replacement;
            assert!(
                serde_json::from_value::<ServiceAuthority>(bad)
                    .map_or(true, |a| a.validate().is_err())
            );
        }
    }
    #[test]
    fn legacy_context_bytes_have_no_platform_and_opaque_context_requires_typed_platform() {
        let value = serde_json::json!({"kind":"theseus_native_integration_context","schema":1,"ownership_id":"original","binding":"11".repeat(16),"manifest_digest":"22".repeat(32),"workspace_marker":"33".repeat(16),"provider_epoch":"44".repeat(32),"integration_manifest_digest":"55".repeat(32),"manager_unique_name":":1.0","namespace_receipt":"original-context"});
        let legacy: IntegrationRuntimeContext = serde_json::from_value(value.clone()).unwrap();
        legacy.validate().unwrap();
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("manager_platform")
                .is_none()
        );
        let mut opaque = value;
        opaque["manager_platform"] = serde_json::json!("launchd");
        opaque["installation_instance_digest"] = serde_json::json!("66".repeat(32));
        opaque["manager_unique_name"] = serde_json::json!("provider-context-1");
        serde_json::from_value::<IntegrationRuntimeContext>(opaque.clone())
            .unwrap()
            .validate()
            .unwrap();
        opaque["manager_unique_name"] = serde_json::json!(":1.0");
        assert!(
            serde_json::from_value::<IntegrationRuntimeContext>(opaque)
                .map_or(true, |c| c.validate().is_err())
        );
    }
}

#[cfg(test)]
mod cooperative_context_tests {
    use super::*;
    #[test]
    fn launchd_private_instance_is_pinned_before_start_by_manifest_and_context() {
        use sha2::{Digest as _, Sha256};
        let fingerprint: Vec<u8> = [1].into_iter().chain([0; 56]).collect();
        let mut value = serde_json::json!({"schema":1,"core_manifest_digest":"11".repeat(32),"service_authority":{"kind":"launchd","slot":"fixture_service","domain":{"kind":"user","uid":501},"label":"org.theseus.fixture","definition_root":"/fixture/plists","exclusive_load_roots":["/fixture/plists"],"launchctl":"/bin/launchctl","qualification":{"os_version":"15.0","os_build":"24A","api_profile":"launchctl-qualified-v1","receipt_sha256":"22".repeat(32),"launchctl_fingerprint":fingerprint},"managed_by":"installer","sut_effects":["registration","configuration","removal"],"allowed_actions":["inspect","start","stop"]},"definitions":[{"role":"configured","artifact":"target","executable":"/fixture/service","arguments":["/fixture/service","provider-private-instance"],"environment":[],"working_directory":"/fixture","definition_sha256":"33".repeat(32)}],"artifact_slots":[],"readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":[],"readiness":{"endpoint":{"kind":"unix_path","path":"/fixture/ready/ready.sock"},"root":"/fixture/ready","peer_uid":501,"peer_gid":20,"cooperative_instance":"provider-private-instance","expectations":[{"role":"configured","artifact":"target","artifact_length":123,"artifact_sha256":"44".repeat(32),"version_literal":"target","data_sha256":"55".repeat(32),"configuration_digest":"66".repeat(32)}]}});
        value["service_authority"]["qualification"]["absence_profile"] = serde_json::json!({"profile":"launchctl-user-no-service-v1","exit_code":113,"stderr":"Bad request.\nCould not find service \"org.theseus.fixture\" in domain for user: 501\n","receipt_sha256":"88".repeat(32)});
        let manifest: IntegrationManifest = serde_json::from_value(value.clone()).unwrap();
        manifest.validate().unwrap();
        // Exact form observed on a real macOS 15.8.2 runner (CI8); near forms stay refused.
        for (stderr, valid) in [
            (
                "Bad request.\nCould not find service \"org.theseus.fixture\" in domain for uid: 501\n",
                true,
            ),
            (
                "Could not find service \"org.theseus.fixture\" in domain for uid: 501\n",
                true,
            ),
            (
                "Bad request.\nCould not find service \"org.theseus.fixture\" in domain for uid: 502\n",
                false,
            ),
            (
                "Bad request.\nCould not find service \"org.theseus.other\" in domain for uid: 501\n",
                false,
            ),
            ("Bad request.\nCould not find domain for uid: 501\n", false),
            (
                "Bad request.\nCould not find service \"org.theseus.fixture\" in domain for gui: 501\n",
                false,
            ),
        ] {
            let mut real = value.clone();
            real["service_authority"]["qualification"]["absence_profile"]["stderr"] = stderr.into();
            assert_eq!(
                serde_json::from_value::<IntegrationManifest>(real)
                    .map_err(|_| ())
                    .and_then(|m| m.validate().map_err(|_| ()))
                    .is_ok(),
                valid,
                "{stderr}"
            );
        }
        let mut context:IntegrationRuntimeContext=serde_json::from_value(serde_json::json!({"kind":"theseus_native_integration_context","schema":1,"ownership_id":"original","binding":"11".repeat(16),"manifest_digest":"22".repeat(32),"workspace_marker":"33".repeat(16),"provider_epoch":"44".repeat(32),"integration_manifest_digest":manifest.digest().unwrap().to_hex(),"manager_unique_name":"provider-context","manager_platform":"launchd","namespace_receipt":"exclusive-original-context","installation_instance_digest":format!("{:x}",Sha256::digest(b"provider-private-instance"))})).unwrap();
        manifest
            .validate_context(&context, super::super::Os::Macos)
            .unwrap();
        assert!(
            manifest
                .validate_context(&context, super::super::Os::Windows)
                .is_err()
        );
        context.installation_instance_digest = Some("77".repeat(32));
        assert!(
            manifest
                .validate_context(&context, super::super::Os::Macos)
                .is_err()
        );
        value["readiness"]
            .as_object_mut()
            .unwrap()
            .remove("cooperative_instance");
        assert!(
            serde_json::from_value::<IntegrationManifest>(value)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}
