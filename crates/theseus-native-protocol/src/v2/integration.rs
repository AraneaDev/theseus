//! Explicit integration profile DTOs. No native handles or implicit endpoints.
mod platform;
use super::{Digest, Error, NativeDescription};
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
pub struct IntegrationCapabilities;
impl IntegrationCapabilities {
    pub const NAMES: [&'static str; 3] = ["inspect_service", "service_start", "service_stop"];
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
#[serde(deny_unknown_fields)]
pub struct IntegrationRuntimeContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_instance_digest: Option<String>,
    #[serde(default, skip_serializing_if = "ManagerPlatform::is_systemd")]
    pub manager_platform: ManagerPlatform,
    pub kind: String,
    pub schema: u32,
    pub ownership_id: String,
    pub binding: String,
    pub manifest_digest: String,
    pub workspace_marker: String,
    pub provider_epoch: String,
    pub integration_manifest_digest: String,
    pub manager_unique_name: String,
    pub namespace_receipt: String,
}
impl IntegrationRuntimeContext {
    pub fn validate(&self) -> Result<(), Error> {
        if self.kind != "theseus_native_integration_context"
            || self.schema != 1
            || self.ownership_id.is_empty()
            || self.ownership_id.len() > 128
            || self.namespace_receipt.is_empty()
            || self.namespace_receipt.len() > 4096
            || self.namespace_receipt.contains('\0')
            || (self.manager_platform == ManagerPlatform::Systemd)
                != self.manager_unique_name.starts_with(':')
            || self.manager_unique_name.is_empty()
            || self.manager_unique_name.len() > 128
            || self.manager_unique_name.contains('\0')
        {
            return Err(Error::InvalidValue);
        }
        if let Some(digest) = &self.installation_instance_digest {
            Digest::parse(digest)?;
        }
        if (self.manager_platform == ManagerPlatform::Systemd)
            != self.installation_instance_digest.is_none()
        {
            return Err(Error::InvalidValue);
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
        Ok(())
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationManifest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness: Option<IntegrationReadiness>,
    pub schema: u32,
    pub core_manifest_digest: String,
    pub service_authority: ServiceAuthority,
    pub definitions: Vec<ServiceDefinition>,
    pub artifact_slots: Vec<IntegrationArtifactSlot>,
    pub readiness_protocol: String,
    pub desktop_capabilities: Vec<String>,
}
impl IntegrationManifest {
    pub fn validate(&self) -> Result<(), Error> {
        if self.schema != 1
            || self.definitions.is_empty()
            || self.definitions.len() > 6
            || self.artifact_slots.len() > 8
            || self.readiness_protocol != "theseus_fixture_ready_v1"
            || !self.desktop_capabilities.is_empty()
            || (self.service_authority.platform() != ManagerPlatform::Systemd
                && self.readiness.is_none())
        {
            return Err(Error::InvalidValue);
        }
        if matches!(&self.service_authority,ServiceAuthority::Launchd{qualification,..} if qualification.absence_profile.is_none())
        {
            return Err(Error::InvalidValue);
        }
        if let Some(readiness) = &self.readiness {
            if !readiness.valid_endpoint(self.service_authority.platform())
                || readiness.expectations.is_empty()
                || readiness.expectations.len() > 6
            {
                return Err(Error::InvalidValue);
            }
            match &self.service_authority {
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
        self.service_authority.validate()?;
        let mut keys = std::collections::BTreeSet::new();
        for d in &self.definitions {
            if !["base", "configured", "target"].contains(&d.role.as_str())
                || !["base", "target"].contains(&d.artifact.as_str())
                || !keys.insert((&d.role, &d.artifact))
                || !platform_absolute(&d.executable, self.service_authority.platform())
                || !platform_absolute(&d.working_directory, self.service_authority.platform())
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
            if self.service_authority.platform() == ManagerPlatform::Scm {
                let raw = d.scm.as_ref().ok_or(Error::InvalidValue)?;
                raw.validate(d, &self.service_authority)?;
                if raw.digest()?.to_hex() != d.definition_sha256 {
                    return Err(Error::InvalidValue);
                }
            } else if d.scm.is_some()
                || (self.service_authority.platform() == ManagerPlatform::Launchd
                    && !d.environment.is_empty())
            {
                return Err(Error::InvalidValue);
            }
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
