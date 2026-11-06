//! Durable original desktop operations in the existing native ledger domain.
//! These serializable records are diagnostic knowledge, never native authority.
mod entry_retirement;
mod generation;
pub use entry_retirement::*;
mod native_lifetime;
mod owned_lifetime;
use super::{
    BindingId, Digest, Error, IntentId, LeaseId, LocalEndpoint, OperationId, ResourceSlot, SidText,
    WorkspaceMarker,
    ledger::{Entry, LedgerError, Record},
};
pub use generation::*;
pub use native_lifetime::*;
pub use owned_lifetime::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

fn digest<T: Serialize>(domain: &[u8], value: &T) -> Result<Digest, Error> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::InvalidValue)?;
    if bytes.len() > super::MAX_BODY_BYTES {
        return Err(Error::LimitExceeded);
    }
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(bytes);
    Ok(Digest::from_bytes(hash.finalize().into()))
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
fn absolute(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 1024
        && !value.contains('\0')
        && !value.split('/').any(|s| matches!(s, "." | ".."))
}
fn windows_absolute(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes.len() <= 1024
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
        && !value.contains('\0')
        && !value.split(['/', '\\']).any(|s| matches!(s, "." | ".."))
}
pub fn validate_endpoint(value: &LocalEndpoint) -> Result<(), Error> {
    let valid = match value {
        LocalEndpoint::UnixPath { path } => {
            absolute(path)
                && path.len() <= 100
                && path
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
        }
        LocalEndpoint::NamedPipe { path } => {
            path.strip_prefix(r"\\.\pipe\theseus.").is_some_and(|name| {
                !name.is_empty()
                    && name.len() <= 64
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
        }
    };
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidValue)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrokerPrincipal {
    Posix {
        effective_uid: u32,
        effective_gid: u32,
    },
    Windows {
        token_user_sid: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopMode {
    OwnedExecutable,
    NativeActivation,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SuppliedDisplay {
    #[cfg(feature = "test-support")]
    SourceOwnedHeadless,
    X11 {
        display: String,
        authority_file: String,
        bus: String,
    },
    Wayland {
        socket: String,
        runtime_directory: String,
        bus: String,
    },
    MacOsGui {
        domain: String,
    },
    WindowsInteractive {
        session_id: u32,
        window_station: String,
        desktop: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationBrokerHeader {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_gio_qualification: Option<super::LinuxGioQualificationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub macos_security_session_id: Option<u32>,
    pub profile: String,
    pub binding: String,
    pub manifest_digest: String,
    pub workspace_marker: String,
    pub os: String,
    pub target_id: String,
    pub ownership_id: String,
    pub provider_epoch: String,
    pub session_id: String,
    pub session_epoch: String,
    pub broker_endpoint: LocalEndpoint,
    pub broker_principal: BrokerPrincipal,
    pub helper_principal: BrokerPrincipal,
    pub display: SuppliedDisplay,
    pub allowed_modes: Vec<DesktopMode>,
}
impl IntegrationBrokerHeader {
    pub fn validate(&self) -> Result<(), Error> {
        if let Some(reference) = &self.linux_gio_qualification {
            reference.validate()?;
            if self.os != "linux" || self.allowed_modes != [DesktopMode::NativeActivation] {
                return Err(Error::InvalidValue);
            }
        }
        BindingId::parse(&self.binding)?;
        WorkspaceMarker::parse(&self.workspace_marker)?;
        let ownership = BindingId::parse(&self.ownership_id)?;
        let provider = Digest::parse(&self.provider_epoch)?;
        let epoch = Digest::parse(&self.session_epoch)?;
        Digest::parse(&self.manifest_digest)?;
        validate_endpoint(&self.broker_endpoint)?;
        if self.profile != "native_integration_broker_v1"
            || !identifier(&self.target_id)
            || !identifier(&self.session_id)
            || ownership.as_bytes() == &[0; 16]
            || provider.as_bytes() == &[0; 32]
            || epoch.as_bytes() == &[0; 32]
            || self.allowed_modes.is_empty()
            || self.allowed_modes.len() > 2
            || self.allowed_modes.windows(2).any(|v| v[0] == v[1])
        {
            return Err(Error::InvalidValue);
        }
        if (self.os == "macos"
            && self.allowed_modes.contains(&DesktopMode::NativeActivation)
            && self.macos_security_session_id.is_none_or(|id| id == 0))
            || (self.macos_security_session_id.is_some()
                && (self.os != "macos" || self.macos_security_session_id == Some(0)))
        {
            return Err(Error::InvalidValue);
        }
        self.authority().validate()
    }
    pub fn authority(&self) -> DesktopAuthority {
        DesktopAuthority {
            target_id: self.target_id.clone(),
            os: self.os.clone(),
            session_id: self.session_id.clone(),
            session_epoch: self.session_epoch.clone(),
            broker_endpoint: self.broker_endpoint.clone(),
            broker_principal: self.broker_principal.clone(),
            helper_principal: self.helper_principal.clone(),
            display: self.display.clone(),
            allowed_modes: self.allowed_modes.clone(),
        }
    }
    pub fn digest(&self) -> Result<Digest, Error> {
        self.validate()?;
        digest(b"theseus-native-broker-header-v1\0", self)
    }
}

/// Separately supplied operator ceiling. Original ownership/provider/header
/// digests arrive in the authenticated per-instance broker handshake, never from
/// a recipe or by discovering the current display/session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopAuthority {
    pub target_id: String,
    pub os: String,
    pub session_id: String,
    pub session_epoch: String,
    pub broker_endpoint: LocalEndpoint,
    pub broker_principal: BrokerPrincipal,
    pub helper_principal: BrokerPrincipal,
    pub display: SuppliedDisplay,
    pub allowed_modes: Vec<DesktopMode>,
}
impl DesktopAuthority {
    pub fn validate(&self) -> Result<(), Error> {
        validate_endpoint(&self.broker_endpoint)?;
        let epoch = Digest::parse(&self.session_epoch)?;
        if !identifier(&self.target_id)
            || !identifier(&self.session_id)
            || epoch.as_bytes() == &[0; 32]
            || self.allowed_modes.is_empty()
            || self.allowed_modes.len() > 2
            || self.allowed_modes.windows(2).any(|m| m[0] == m[1])
        {
            return Err(Error::InvalidValue);
        }
        let principals = match (
            &self.broker_principal,
            &self.helper_principal,
            &self.broker_endpoint,
            self.os.as_str(),
        ) {
            (
                BrokerPrincipal::Posix { .. },
                BrokerPrincipal::Posix { .. },
                LocalEndpoint::UnixPath { .. },
                "linux" | "macos",
            ) => true,
            (
                BrokerPrincipal::Windows { token_user_sid: b },
                BrokerPrincipal::Windows { token_user_sid: h },
                LocalEndpoint::NamedPipe { .. },
                "windows",
            ) => SidText::new(b).is_ok() && SidText::new(h).is_ok(),
            _ => false,
        };
        let display = match (&self.display, self.os.as_str()) {
            #[cfg(feature = "test-support")]
            (SuppliedDisplay::SourceOwnedHeadless, "linux" | "macos" | "windows") => {
                self.allowed_modes == [DesktopMode::OwnedExecutable]
            }
            (
                SuppliedDisplay::X11 {
                    display,
                    authority_file,
                    bus,
                },
                "linux",
            ) => {
                let number = display.strip_prefix(':');
                number.is_some_and(|v| {
                    let parts: Vec<_> = v.split('.').collect();
                    parts.len() <= 2
                        && parts
                            .iter()
                            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                }) && display.len() <= 32
                    && absolute(authority_file)
                    && validate_endpoint(&LocalEndpoint::UnixPath { path: bus.clone() }).is_ok()
            }
            (
                SuppliedDisplay::Wayland {
                    socket,
                    runtime_directory,
                    bus,
                },
                "linux",
            ) => {
                absolute(runtime_directory)
                    && validate_endpoint(&LocalEndpoint::UnixPath {
                        path: socket.clone(),
                    })
                    .is_ok()
                    && validate_endpoint(&LocalEndpoint::UnixPath { path: bus.clone() }).is_ok()
            }
            (SuppliedDisplay::MacOsGui { domain }, "macos") => domain
                .strip_prefix("gui/")
                .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())),
            (
                SuppliedDisplay::WindowsInteractive {
                    session_id,
                    window_station,
                    desktop,
                },
                "windows",
            ) => *session_id != 0 && window_station == "WinSta0" && desktop == "Default",
            _ => false,
        };
        if principals && display {
            Ok(())
        } else {
            Err(Error::InvalidValue)
        }
    }
    pub fn validate_header(&self, header: &IntegrationBrokerHeader) -> Result<(), Error> {
        self.validate()?;
        header.validate()?;
        if self != &header.authority() {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
}

/// Static desktop-only ceiling; it needs no service authority or bus manager.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopManifest {
    pub schema: u8,
    pub core_manifest_digest: String,
    pub slot: ResourceSlot,
    pub entry_root: String,
    /// Operator/SUT promise that this entry mapping is exclusive and quiescent.
    /// This does not enforce immutable files against a hostile same-UID writer.
    pub namespace_policy: String,
    pub entry: String,
    pub entry_digest: String,
    pub executable_digest: String,
    pub allowed_modes: Vec<DesktopMode>,
    pub ready_endpoint: LocalEndpoint,
    pub close_endpoint: LocalEndpoint,
    pub expectation_digest: String,
}
impl DesktopManifest {
    pub fn validate(&self) -> Result<(), Error> {
        ResourceSlot::new(self.slot.as_str())?;
        for value in [
            &self.core_manifest_digest,
            &self.entry_digest,
            &self.executable_digest,
            &self.expectation_digest,
        ] {
            Digest::parse(value)?;
        }
        validate_endpoint(&self.ready_endpoint)?;
        validate_endpoint(&self.close_endpoint)?;
        let root_matches = match (&self.ready_endpoint, &self.close_endpoint) {
            (LocalEndpoint::UnixPath { .. }, LocalEndpoint::UnixPath { .. }) => {
                absolute(&self.entry_root)
            }
            (LocalEndpoint::NamedPipe { .. }, LocalEndpoint::NamedPipe { .. }) => {
                windows_absolute(&self.entry_root)
            }
            _ => false,
        };
        if self.schema != 1
            || self.namespace_policy != "exclusive_quiescent"
            || !root_matches
            || !identifier(&self.entry)
            || self.allowed_modes.is_empty()
            || self.allowed_modes.len() > 2
            || self.allowed_modes.windows(2).any(|v| v[0] == v[1])
            || self.ready_endpoint == self.close_endpoint
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<Digest, Error> {
        self.validate()?;
        digest(b"theseus-native-desktop-manifest-v1\0", self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
// The single bounded original-generation witness remains inline to preserve
// the public action shape; at most MAX_OPERATIONS accepted requests are retained.
#[allow(clippy::large_enum_variant)]
pub enum DesktopAction {
    Activate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owned_lifetime: Option<OwnedDesktopLifetimeDefinition>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transition: Option<DesktopActivationTransition>,
        mode: DesktopMode,
        entry_digest: String,
        expectation_digest: String,
        close_binding: CooperativeCloseBinding,
    },
    RequestClose {
        original_activation_operation: String,
        nonce: String,
        expectation_digest: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CooperativeCloseBinding {
    pub operation: String,
    pub action_digest: String,
    pub nonce: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopRequest {
    pub helper_lease: String,
    pub helper_operation: String,
    pub broker_lease: String,
    pub binding_digest: String,
    pub activation_nonce: String,
    pub slot: ResourceSlot,
    pub prior_binding_digest: String,
    pub action: DesktopAction,
}
impl DesktopRequest {
    pub fn validate(&self) -> Result<(), Error> {
        LeaseId::parse(&self.helper_lease)?;
        LeaseId::parse(&self.broker_lease)?;
        OperationId::parse(&self.helper_operation)?;
        Digest::parse(&self.binding_digest)?;
        Digest::parse(&self.prior_binding_digest)?;
        if BindingId::parse(&self.activation_nonce)?.as_bytes() == &[0; 16] {
            return Err(Error::InvalidValue);
        }
        ResourceSlot::new(self.slot.as_str())?;
        if let Some(transition) = self.transition() {
            transition.validate_request(self)?;
        }
        match &self.action {
            DesktopAction::Activate {
                owned_lifetime,
                mode,
                entry_digest,
                expectation_digest,
                close_binding,
                ..
            } => {
                if let Some(definition) = owned_lifetime {
                    definition.validate_mode(&[*mode])?;
                }
                Digest::parse(entry_digest)?;
                Digest::parse(expectation_digest)?;
                OperationId::parse(&close_binding.operation)?;
                Digest::parse(&close_binding.action_digest)?;
                if close_binding.operation == self.helper_operation
                    || close_binding.nonce == self.activation_nonce
                    || self.close_request()?.digest()?.to_hex() != close_binding.action_digest
                {
                    return Err(Error::InvalidValue);
                }
            }
            DesktopAction::RequestClose {
                original_activation_operation,
                nonce,
                expectation_digest,
            } => {
                OperationId::parse(original_activation_operation)?;
                Digest::parse(expectation_digest)?;
                if original_activation_operation == &self.helper_operation
                    || BindingId::parse(nonce)?.as_bytes() == &[0; 16]
                    || nonce == &self.activation_nonce
                {
                    return Err(Error::InvalidValue);
                }
            }
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<Digest, Error> {
        self.validate()?;
        digest(b"theseus-native-broker-request-v1\0", self)
    }
    /// The close digest has no dependency on the enclosing Activate digest.
    pub fn close_request(&self) -> Result<Self, Error> {
        let DesktopAction::Activate {
            expectation_digest,
            close_binding,
            ..
        } = &self.action
        else {
            return Err(Error::InvalidValue);
        };
        let mut close = self.clone();
        close.helper_operation = close_binding.operation.clone();
        close.action = DesktopAction::RequestClose {
            original_activation_operation: self.helper_operation.clone(),
            nonce: close_binding.nonce.clone(),
            expectation_digest: expectation_digest.clone(),
        };
        Ok(close)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerOperationIdentity {
    pub helper_lease: String,
    pub helper_operation: String,
    pub broker_lease: String,
    pub request_digest: String,
    pub binding_digest: String,
    pub activation_nonce: String,
}
impl BrokerOperationIdentity {
    pub fn validate(&self) -> Result<(), Error> {
        LeaseId::parse(&self.helper_lease)?;
        LeaseId::parse(&self.broker_lease)?;
        OperationId::parse(&self.helper_operation)?;
        Digest::parse(&self.request_digest)?;
        Digest::parse(&self.binding_digest)?;
        if BindingId::parse(&self.activation_nonce)?.as_bytes() == &[0; 16] {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopObservation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<DesktopActivationTransition>,
    pub slot: ResourceSlot,
    pub entry_present: bool,
    pub entry_valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable_digest: Option<String>,
    pub session_header_digest: String,
    pub binding_digest: String,
    pub complete: bool,
}
impl DesktopObservation {
    pub fn validate(&self) -> Result<(), Error> {
        ResourceSlot::new(self.slot.as_str())?;
        if let Some(generation) = &self.generation {
            generation.validate()?;
        }
        Digest::parse(&self.session_header_digest)?;
        Digest::parse(&self.binding_digest)?;
        for digest in [&self.entry_digest, &self.executable_digest]
            .into_iter()
            .flatten()
        {
            Digest::parse(digest)?;
        }
        if self.complete && (self.entry_present != self.entry_digest.is_some()) {
            return Err(Error::InvalidValue);
        }
        if self.entry_valid
            && (!self.entry_present
                || self.entry_digest.is_none()
                || self.executable_digest.is_none())
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
}
impl DesktopRequest {
    pub fn identity(&self) -> Result<BrokerOperationIdentity, Error> {
        Ok(BrokerOperationIdentity {
            helper_lease: self.helper_lease.clone(),
            helper_operation: self.helper_operation.clone(),
            broker_lease: self.broker_lease.clone(),
            request_digest: self.digest()?.to_hex(),
            binding_digest: self.binding_digest.clone(),
            activation_nonce: self.activation_nonce.clone(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentPublicationScope {
    FileAndDirectorySynced,
    FileFlushedAtomicReplace,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerBind {
    pub ownership_id: String,
    pub helper_lease: String,
    pub helper_operation: String,
    pub slot: ResourceSlot,
    pub broker_intent: String,
    pub expected_header: IntegrationBrokerHeader,
}
impl BrokerBind {
    pub fn validate(&self) -> Result<(), Error> {
        BindingId::parse(&self.ownership_id)?;
        LeaseId::parse(&self.helper_lease)?;
        OperationId::parse(&self.helper_operation)?;
        ResourceSlot::new(self.slot.as_str())?;
        IntentId::parse(&self.broker_intent)?;
        self.expected_header.validate()?;
        if self.ownership_id != self.expected_header.ownership_id {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<Digest, Error> {
        self.validate()?;
        digest(b"theseus-native-broker-bind-v1\0", self)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerBindingReceipt {
    pub original: BrokerBind,
    pub request_digest: String,
    pub publication_scope: IntentPublicationScope,
    pub broker_lease: String,
}
impl BrokerBindingReceipt {
    pub fn validate(&self) -> Result<(), Error> {
        self.original.validate()?;
        LeaseId::parse(&self.broker_lease)?;
        if self.request_digest != self.original.digest()?.to_hex() {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopEvidence {
    OwnedExecutable,
    NativeActivation,
    SourceOwnedExecutable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerEffectReceipt {
    pub evidence: DesktopEvidence,
    pub acknowledged: bool,
    pub receipt_digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrokerEffect {
    Unknown,
    Receipt { receipt: BrokerEffectReceipt },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerOwner {
    Retained,
    Settled,
    Lost,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerPrimary {
    Running,
    Exited,
    NotCreated,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerDescendants {
    Pending,
    Joined,
    NotCreated,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_lifetime: Option<OwnedLifetimeState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_lifetime: Option<NativeLifetimeState>,
    pub effect: BrokerEffect,
    pub owner: BrokerOwner,
    pub primary: BrokerPrimary,
    pub descendants: BrokerDescendants,
    pub cleanup_complete: bool,
    pub quarantined: bool,
}
impl BrokerState {
    pub fn validate(&self) -> Result<(), Error> {
        self.validate_owned_lifetime()?;
        if let Some(native) = &self.native_lifetime {
            native.validate()?;
            if self.cleanup_complete
                || (self.primary == BrokerPrimary::Exited
                    && native.primary != NativePrimaryState::Exited)
                || self.descendants != BrokerDescendants::Unknown
                || (matches!(&self.effect,BrokerEffect::Receipt {receipt} if receipt.evidence!=DesktopEvidence::NativeActivation))
            {
                return Err(Error::InvalidValue);
            }
        }
        if let BrokerEffect::Receipt { receipt } = &self.effect {
            Digest::parse(&receipt.receipt_digest)?;
            if receipt.evidence == DesktopEvidence::NativeActivation
                && self.native_lifetime.is_none()
                && (self.cleanup_complete || self.owner == BrokerOwner::Settled)
            {
                return Err(Error::InvalidValue);
            }
        }
        if self.cleanup_complete
            && (self.owner != BrokerOwner::Settled
                || !matches!(
                    self.primary,
                    BrokerPrimary::Exited | BrokerPrimary::NotCreated
                )
                || !matches!(
                    self.descendants,
                    BrokerDescendants::Joined | BrokerDescendants::NotCreated
                ))
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn unknown() -> Self {
        Self {
            owned_lifetime: None,
            native_lifetime: None,
            effect: BrokerEffect::Unknown,
            owner: BrokerOwner::Lost,
            primary: BrokerPrimary::Unknown,
            descendants: BrokerDescendants::Unknown,
            cleanup_complete: false,
            quarantined: true,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrokerEvent {
    EntryPreparationAccepted {
        preparation: EntryPreparation,
    },
    EntryPreparationSettled {
        operation: String,
    },
    EntryProofQuarantined {},
    HelperSutRunCompleted {
        witness: DesktopSutRunWitness,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        definition: Option<super::DesktopDefinitionKey>,
    },
    HelperTransitionAccepted {
        identity: BrokerOperationIdentity,
        transition: DesktopActivationTransition,
    },
    HelperActionAccepted {
        request: DesktopRequest,
        request_digest: String,
    },
    HelperStateObserved {
        identity: BrokerOperationIdentity,
        state: BrokerState,
    },
    HelperBrokerReleased {
        helper_lease: String,
        broker_lease: String,
    },
    BindAccepted {
        original: BrokerBind,
        request_digest: String,
    },
    BindIntentPublished {
        operation: String,
        scope: IntentPublicationScope,
    },
    BindingCommitted {
        receipt: BrokerBindingReceipt,
    },
    Accepted {
        request: DesktopRequest,
        request_digest: String,
    },
    EffectRecorded {
        operation: String,
        receipt: BrokerEffectReceipt,
    },
    OwnedLifetimeRecorded {
        operation: String,
        state: OwnedLifetimeState,
    },
    NativeLifetimeRecorded {
        operation: String,
        state: NativeLifetimeState,
    },
    LifetimeRecorded {
        operation: String,
        primary: BrokerPrimary,
        descendants: BrokerDescendants,
        cleanup_complete: bool,
    },
    Quarantined {
        operation: String,
    },
}
#[derive(Clone, Debug)]
struct Accepted {
    request: DesktopRequest,
    state: BrokerState,
    receipt: bool,
    quarantined: bool,
}
/// A projection over the SAME LedgerStore records. Its in-memory table grants no
/// launch authority: all reopened incomplete native owners are lost.
#[derive(Clone, Debug, Default)]
pub struct BrokerLedgerView {
    entry_preparations: Vec<(EntryPreparation, bool)>,
    entry_quarantined: bool,
    sut_runs: Vec<(DesktopSutRunWitness, Option<super::DesktopDefinitionKey>)>,
    sut_accepted_at: Vec<(String, u16)>,
    helper_retired_at: Vec<(String, u16)>,
    transitions: Vec<(BrokerOperationIdentity, DesktopActivationTransition)>,
    accepted: Vec<Accepted>,
    helper_actions: Vec<(DesktopRequest, Option<BrokerState>)>,
    helper_released: bool,
    binds: Vec<(
        BrokerBind,
        Option<IntentPublicationScope>,
        Option<BrokerBindingReceipt>,
    )>,
}
/// Verify the helper's original declared desktop ceiling against its complete
/// immutable integration header. An ordinary service-only IntegrationHeader
/// grants no desktop capability. No serialized declaration creates native owners.
pub fn declared_desktop(
    records: &[Record],
) -> Result<Option<(&DesktopManifest, &IntegrationBrokerHeader)>, LedgerError> {
    let declaration = records
        .iter()
        .filter(|r| matches!(r.entry(), Entry::DesktopAuthorityDeclared { .. }))
        .collect::<Vec<_>>();
    if declaration.is_empty() {
        return Ok(None);
    }
    if declaration.len() != 1 || declaration[0].index() != 1 {
        return Err(LedgerError::InvalidTransition);
    }
    let Entry::DesktopAuthorityDeclared { manifest, context } = declaration[0].entry() else {
        unreachable!()
    };
    let Some(Entry::IntegrationHeader {
        binding,
        manifest_digest,
        workspace_marker,
        os,
        provider_epoch,
        core_manifest_digest,
        integration_manifest_digest,
        context_digest,
        ownership_id,
        ..
    }) = records.first().map(Record::entry)
    else {
        return Err(LedgerError::InvalidTransition);
    };
    manifest
        .validate_context(context, *os)
        .map_err(|_| LedgerError::InvalidTransition)?;
    if manifest.core_manifest_digest != core_manifest_digest.to_hex()
        || manifest
            .digest()
            .map_err(|_| LedgerError::InvalidTransition)?
            != *integration_manifest_digest
        || super::integration_manifest_digest(*core_manifest_digest, *integration_manifest_digest)
            != *manifest_digest
        || context
            .digest()
            .map_err(|_| LedgerError::InvalidTransition)?
            != *context_digest
        || context.binding != binding.to_hex()
        || context.manifest_digest != manifest_digest.to_hex()
        || context.workspace_marker != workspace_marker.to_hex()
        || context.provider_epoch != provider_epoch.to_hex()
        || context.ownership_id != *ownership_id
    {
        return Err(LedgerError::InvalidTransition);
    }
    Ok(Some((
        manifest
            .desktop
            .as_ref()
            .ok_or(LedgerError::InvalidTransition)?,
        context
            .desktop_broker_header
            .as_ref()
            .ok_or(LedgerError::InvalidTransition)?,
    )))
}
impl BrokerLedgerView {
    pub fn entry_preparations(&self) -> &[(EntryPreparation, bool)] {
        &self.entry_preparations
    }
    pub fn entry_blocked(&self) -> bool {
        self.entry_quarantined || self.entry_preparations.iter().any(|(_, settled)| !settled)
    }
    pub fn entry_quarantined(&self) -> bool {
        self.entry_quarantined
    }
    pub fn replay(records: &[Record]) -> Result<Self, LedgerError> {
        let mut result = Self::default();
        let broker_header = match records.first().map(Record::entry) {
            Some(Entry::IntegrationBrokerHeader { header }) => {
                header.validate().map_err(|_| LedgerError::InvalidRecord)?;
                Some(header)
            }
            _ => None,
        };
        let declared = declared_desktop(records)?;
        let qualification = super::declared_linux_gio_qualification(records)?;
        let helper = matches!(
            records.first().map(Record::entry),
            Some(Entry::IntegrationHeader { .. })
        );
        let lease = records.iter().find_map(|r| match r.entry() {
            Entry::LeaseReserved { lease, .. } => Some(lease.to_hex()),
            _ => None,
        });
        for (index, record) in records.iter().enumerate() {
            if usize::from(record.index()) != index {
                return Err(LedgerError::InvalidTransition);
            }
            let Entry::Broker { event } = record.entry() else {
                if matches!(record.entry(), Entry::LeaseClosed { .. }) && result.entry_blocked() {
                    return Err(LedgerError::InvalidTransition);
                }
                continue;
            };
            let invalid = || LedgerError::InvalidTransition;
            match event {
                BrokerEvent::EntryPreparationAccepted { preparation } => {
                    preparation.validate().map_err(|_| invalid())?;
                    let header = broker_header.ok_or_else(invalid)?;
                    if result.entry_blocked()
                        || result.accepted.iter().any(|a| !a.state.original_retired())
                        || lease.as_deref() != Some(&preparation.broker_lease)
                        || header.digest().map_err(|_| invalid())?.to_hex()
                            != preparation.header_digest
                        || result.entry_preparations.len() >= super::MAX_OPERATIONS
                        || result
                            .entry_preparations
                            .iter()
                            .any(|(p, _)| p.operation == preparation.operation)
                    {
                        return Err(invalid());
                    }
                    result.entry_preparations.push((preparation.clone(), false));
                }
                BrokerEvent::EntryPreparationSettled { operation } => {
                    if broker_header.is_none() || result.entry_quarantined {
                        return Err(invalid());
                    }
                    let (_, settled) = result
                        .entry_preparations
                        .iter_mut()
                        .find(|(p, _)| &p.operation == operation)
                        .ok_or_else(invalid)?;
                    if *settled {
                        return Err(invalid());
                    }
                    *settled = true;
                }
                BrokerEvent::EntryProofQuarantined {} => {
                    if broker_header.is_none() {
                        return Err(invalid());
                    }
                    result.entry_quarantined = true;
                }
                BrokerEvent::HelperSutRunCompleted {
                    witness,
                    definition,
                } => {
                    if !helper || result.helper_released || declared.is_none() {
                        return Err(invalid());
                    }
                    witness
                        .matches_successful_terminal(&records[..index])
                        .map_err(|_| invalid())?;
                    let previous = result.sut_runs.iter().rev().find_map(|(_, key)| *key);
                    if witness.definition_after(previous).map_err(|_| invalid())? != *definition
                        || result
                            .sut_runs
                            .iter()
                            .any(|(old, _)| old.operation == witness.operation)
                    {
                        return Err(invalid());
                    }
                    let accepted=records[..usize::from(record.index())].iter().find(|r|matches!(r.entry(),Entry::OperationAccepted {operation,request_digest,operation_kind:super::OperationKind::Run,..} if operation.to_hex()==witness.operation && request_digest.to_hex()==witness.request_digest)).ok_or_else(invalid)?;
                    result
                        .sut_accepted_at
                        .push((witness.operation.clone(), accepted.index()));
                    result.sut_runs.push((witness.clone(), *definition));
                }
                BrokerEvent::HelperTransitionAccepted {
                    identity,
                    transition,
                } => {
                    if !helper || result.helper_released {
                        return Err(invalid());
                    }
                    let request = result
                        .helper_actions
                        .iter()
                        .find(|(r, _)| r.identity().as_ref() == Ok(identity))
                        .map(|(r, _)| r)
                        .ok_or_else(invalid)?;
                    if request.transition() != Some(transition)
                        || result
                            .transitions
                            .iter()
                            .any(|(old, _)| old.helper_operation == identity.helper_operation)
                    {
                        return Err(invalid());
                    }
                    result
                        .transitions
                        .push((identity.clone(), transition.clone()));
                }
                BrokerEvent::HelperActionAccepted {
                    request,
                    request_digest,
                } => {
                    request.validate().map_err(|_| invalid())?;
                    let (desktop, header) = declared.ok_or_else(invalid)?;
                    let receipt = result
                        .binds
                        .first()
                        .and_then(|b| b.2.as_ref())
                        .ok_or_else(invalid)?;
                    if !helper
                        || result.helper_released
                        || request.slot != desktop.slot
                        || request.helper_lease != receipt.original.helper_lease
                        || request.broker_lease != receipt.broker_lease
                        || request.binding_digest
                            != header.digest().map_err(|_| invalid())?.to_hex()
                        || request.digest().map_err(|_| invalid())?.to_hex() != *request_digest
                        || result
                            .helper_actions
                            .iter()
                            .any(|(r, _)| r.helper_operation == request.helper_operation)
                    {
                        return Err(invalid());
                    }
                    match &request.action {
                        DesktopAction::Activate { .. } => {
                            let manifest = records
                                .iter()
                                .find_map(|r| match r.entry() {
                                    Entry::DesktopAuthorityDeclared { manifest, .. } => {
                                        Some(manifest.as_ref())
                                    }
                                    _ => None,
                                })
                                .ok_or_else(invalid)?;
                            result
                                .validate_helper_generation(manifest, request)
                                .map_err(|_| invalid())?;
                        }
                        DesktopAction::RequestClose { .. }
                            if result
                                .helper_actions
                                .iter()
                                .any(|(r, _)| r.close_request().as_ref() == Ok(request)) => {}
                        _ => return Err(invalid()),
                    }
                    let initial = match &request.action {
                        DesktopAction::Activate {
                            owned_lifetime: Some(definition),
                            ..
                        } => {
                            definition
                                .validate_platform(&header.os)
                                .map_err(|_| invalid())?;
                            Some(BrokerState::accepted_owned(*definition))
                        }
                        DesktopAction::RequestClose {
                            original_activation_operation,
                            ..
                        } => result
                            .helper_actions
                            .iter()
                            .find(|(r, _)| &r.helper_operation == original_activation_operation)
                            .and_then(|(_, state)| state.as_ref())
                            .filter(|state| state.owned_lifetime.is_some())
                            .map(|original| {
                                let owned = original
                                    .owned_lifetime
                                    .as_ref()
                                    .ok_or(Error::InvalidValue)?;
                                let mut state = BrokerState::accepted_owned(owned.definition);
                                state.record_owned_lifetime(owned.clone())?;
                                state.owner = original.owner;
                                state.quarantined = original.quarantined;
                                Ok::<_, Error>(state)
                            })
                            .transpose()
                            .map_err(|_| invalid())?,
                        _ => None,
                    };
                    result.helper_actions.push((request.clone(), initial));
                }
                BrokerEvent::HelperStateObserved { identity, state } => {
                    identity.validate().map_err(|_| invalid())?;
                    state.validate().map_err(|_| invalid())?;
                    if result.helper_actions.iter().any(|(request, _)| {
                        request.identity().as_ref() == Ok(identity)
                            && request.transition().is_some()
                    }) && result.helper_transition(identity).is_none()
                    {
                        return Err(invalid());
                    }
                    if !helper || result.helper_released {
                        return Err(invalid());
                    }
                    let entry = result
                        .helper_actions
                        .iter_mut()
                        .find(|(r, _)| r.identity().as_ref() == Ok(identity))
                        .ok_or_else(invalid)?;
                    if let Some(old) = &entry.1
                        && ((matches!(old.effect, BrokerEffect::Receipt { .. })
                            && old.effect != state.effect)
                            || (old.primary == BrokerPrimary::Exited
                                && state.primary != BrokerPrimary::Exited)
                            || (old.descendants == BrokerDescendants::Joined
                                && state.descendants != BrokerDescendants::Joined)
                            || (old.cleanup_complete && !state.cleanup_complete)
                            || (old.quarantined && !state.quarantined))
                    {
                        return Err(invalid());
                    }
                    match (
                        entry.1.as_ref().and_then(|s| s.owned_lifetime.as_ref()),
                        &state.owned_lifetime,
                    ) {
                        (Some(old), Some(next)) => {
                            next.validate_platform(&declared.ok_or_else(invalid)?.1.os)
                                .map_err(|_| invalid())?;
                            next.validate_progress_from(old).map_err(|_| invalid())?;
                            if entry
                                .1
                                .as_ref()
                                .is_some_and(|s| s.owner == BrokerOwner::Lost)
                                && state.owner != BrokerOwner::Lost
                            {
                                return Err(invalid());
                            }
                        }
                        (None, None) => {}
                        _ => return Err(invalid()),
                    }
                    if let Some(native) = &state.native_lifetime {
                        let manifest = records
                            .iter()
                            .find_map(|r| match r.entry() {
                                Entry::DesktopAuthorityDeclared { manifest, .. } => Some(manifest),
                                _ => None,
                            })
                            .ok_or_else(invalid)?;
                        let key = entry.0.transition().map_or(
                            super::DesktopDefinitionKey::CONFIGURED_TARGET,
                            |transition| transition.definition,
                        );
                        manifest
                            .desktop_definition(&key)
                            .map_err(|_| invalid())?
                            .native
                            .as_ref()
                            .ok_or_else(invalid)?
                            .validate_state(native)
                            .map_err(|_| invalid())?;
                    }
                    if let Some(old) = &entry.1 {
                        match (&old.native_lifetime, &state.native_lifetime) {
                            (Some(prior), Some(next)) => {
                                next.validate_progress_from(prior).map_err(|_| invalid())?
                            }
                            (Some(_), None) => return Err(invalid()),
                            (None, Some(_))
                                if old.owner == BrokerOwner::Lost || old.quarantined =>
                            {
                                return Err(invalid());
                            }
                            _ => {}
                        }
                    }
                    if state.original_retired()
                        && !result
                            .helper_retired_at
                            .iter()
                            .any(|(operation, _)| operation == &identity.helper_operation)
                    {
                        result
                            .helper_retired_at
                            .push((identity.helper_operation.clone(), record.index()));
                    }
                    entry.1 = Some(state.clone());
                }
                BrokerEvent::HelperBrokerReleased {
                    helper_lease,
                    broker_lease,
                } => {
                    let receipt = result
                        .binds
                        .first()
                        .and_then(|b| b.2.as_ref())
                        .ok_or_else(invalid)?;
                    if !helper
                        || result.helper_released
                        || &receipt.original.helper_lease != helper_lease
                        || &receipt.broker_lease != broker_lease
                        || result
                            .helper_actions
                            .iter()
                            .any(|(_, s)| s.as_ref().is_none_or(|s| !s.original_retired()))
                    {
                        return Err(invalid());
                    }
                    result.helper_released = true;
                }
                BrokerEvent::BindAccepted {
                    original,
                    request_digest,
                } => {
                    original.validate().map_err(|_| invalid())?;
                    if original.expected_header.linux_gio_qualification.is_some()
                        && qualification.is_none()
                    {
                        return Err(invalid());
                    }
                    let header_matches = matches!(records.first().map(Record::entry), Some(Entry::IntegrationHeader { manifest_digest, provider_epoch, ownership_id, .. })
                        if original.expected_header.manifest_digest == manifest_digest.to_hex() && original.expected_header.provider_epoch == provider_epoch.to_hex() && original.ownership_id == *ownership_id);
                    let declared_matches = declared.is_some_and(|(desktop, header)| {
                        header == &original.expected_header && desktop.slot == original.slot
                    });
                    if !helper
                        || !declared_matches
                        || !header_matches
                        || lease.as_ref() != Some(&original.helper_lease)
                        || original.digest().map_err(|_| invalid())?.to_hex() != *request_digest
                        || !result.binds.is_empty()
                    {
                        return Err(invalid());
                    }
                    result.binds.push((original.clone(), None, None));
                }
                BrokerEvent::BindIntentPublished { operation, scope } => {
                    let entry = result
                        .binds
                        .iter_mut()
                        .find(|(b, s, r)| {
                            b.helper_operation == *operation && s.is_none() && r.is_none()
                        })
                        .ok_or_else(invalid)?;
                    entry.1 = Some(*scope);
                }
                BrokerEvent::BindingCommitted { receipt } => {
                    receipt.validate().map_err(|_| invalid())?;
                    let entry = result
                        .binds
                        .iter_mut()
                        .find(|(b, s, r)| {
                            b == &receipt.original
                                && *s == Some(receipt.publication_scope)
                                && r.is_none()
                        })
                        .ok_or_else(invalid)?;
                    entry.2 = Some(receipt.clone());
                }
                BrokerEvent::Accepted {
                    request,
                    request_digest,
                } => {
                    request.validate().map_err(|_| invalid())?;
                    let header = broker_header.ok_or_else(invalid)?;
                    if lease.as_ref() != Some(&request.broker_lease)
                        || request.binding_digest
                            != header.digest().map_err(|_| invalid())?.to_hex()
                        || request.digest().map_err(|_| invalid())?.to_hex() != *request_digest
                        || result.accepted.len() >= super::MAX_OPERATIONS
                        || result.accepted.iter().any(|a| {
                            a.request.helper_operation == request.helper_operation || a.quarantined
                        })
                    {
                        return Err(invalid());
                    }
                    match &request.action {
                        DesktopAction::Activate {
                            mode,
                            owned_lifetime,
                            ..
                        } => {
                            if let Some(definition) = owned_lifetime {
                                definition
                                    .validate_platform(&header.os)
                                    .map_err(|_| invalid())?;
                            }
                            if !header.allowed_modes.contains(mode)
                                || result.validate_broker_generation(request).is_err()
                            {
                                return Err(invalid());
                            }
                        }
                        DesktopAction::RequestClose {
                            original_activation_operation,
                            ..
                        } => {
                            if !result.accepted.iter().any(|a| {
                                a.request.helper_operation == *original_activation_operation
                                    && a.request.close_request().as_ref() == Ok(request)
                            }) {
                                return Err(invalid());
                            }
                        }
                    }
                    let state = match &request.action {
                        DesktopAction::Activate {
                            owned_lifetime: Some(definition),
                            ..
                        } => BrokerState::accepted_owned(*definition),
                        DesktopAction::RequestClose {
                            original_activation_operation,
                            ..
                        } => {
                            let original = result
                                .accepted
                                .iter()
                                .find(|a| {
                                    &a.request.helper_operation == original_activation_operation
                                })
                                .ok_or_else(invalid)?;
                            if let Some(owned) = &original.state.owned_lifetime {
                                let mut state = BrokerState::accepted_owned(owned.definition);
                                state
                                    .record_owned_lifetime(owned.clone())
                                    .map_err(|_| invalid())?;
                                state.quarantined = original.state.quarantined;
                                state
                            } else {
                                BrokerState::unknown()
                            }
                        }
                        _ => BrokerState::unknown(),
                    };
                    result.accepted.push(Accepted {
                        request: request.clone(),
                        state,
                        receipt: false,
                        quarantined: false,
                    });
                }
                BrokerEvent::EffectRecorded { operation, receipt } => {
                    Digest::parse(&receipt.receipt_digest).map_err(|_| invalid())?;
                    let original_native = result
                        .accepted
                        .iter()
                        .find(|a| a.request.helper_operation == *operation)
                        .and_then(|a| match &a.request.action {
                            DesktopAction::RequestClose {
                                original_activation_operation,
                                ..
                            } => Some(original_activation_operation),
                            _ => None,
                        })
                        .is_some_and(|original| {
                            result.accepted.iter().any(|a| {
                                a.request.helper_operation == *original
                                    && matches!(
                                        a.request.action,
                                        DesktopAction::Activate {
                                            mode: DesktopMode::NativeActivation,
                                            ..
                                        }
                                    )
                            })
                        });
                    let accepted = result
                        .accepted
                        .iter_mut()
                        .find(|a| a.request.helper_operation == *operation)
                        .ok_or_else(invalid)?;
                    let evidence_matches = match &accepted.request.action {
                        DesktopAction::Activate {
                            mode: DesktopMode::OwnedExecutable,
                            ..
                        } => {
                            #[cfg(feature = "test-support")]
                            let source = broker_header
                                .is_some_and(|h| h.display == SuppliedDisplay::SourceOwnedHeadless);
                            #[cfg(not(feature = "test-support"))]
                            let source = false;
                            receipt.evidence
                                == if source {
                                    DesktopEvidence::SourceOwnedExecutable
                                } else {
                                    DesktopEvidence::OwnedExecutable
                                }
                        }
                        DesktopAction::Activate {
                            mode: DesktopMode::NativeActivation,
                            ..
                        } => receipt.evidence == DesktopEvidence::NativeActivation,
                        DesktopAction::RequestClose { .. } if original_native => {
                            receipt.evidence == DesktopEvidence::NativeActivation
                        }
                        DesktopAction::RequestClose { .. } => {
                            #[cfg(feature = "test-support")]
                            let source = broker_header
                                .is_some_and(|h| h.display == SuppliedDisplay::SourceOwnedHeadless);
                            #[cfg(not(feature = "test-support"))]
                            let source = false;
                            receipt.evidence
                                == if source {
                                    DesktopEvidence::SourceOwnedExecutable
                                } else {
                                    DesktopEvidence::OwnedExecutable
                                }
                        }
                    };
                    if accepted.receipt || accepted.quarantined || !evidence_matches {
                        return Err(invalid());
                    }
                    accepted.receipt = true;
                    accepted.state.effect = BrokerEffect::Receipt {
                        receipt: receipt.clone(),
                    };
                }
                BrokerEvent::OwnedLifetimeRecorded { operation, state } => {
                    state
                        .validate_platform(&broker_header.ok_or_else(invalid)?.os)
                        .map_err(|_| invalid())?;
                    let accepted = result
                        .accepted
                        .iter_mut()
                        .find(|a| a.request.helper_operation == *operation)
                        .ok_or_else(invalid)?;
                    if !matches!(&accepted.request.action, DesktopAction::Activate { mode: DesktopMode::OwnedExecutable, owned_lifetime: Some(definition), .. } if definition == &state.definition)
                    {
                        return Err(invalid());
                    }
                    accepted
                        .state
                        .record_owned_lifetime(state.clone())
                        .map_err(|_| invalid())?;
                    accepted.state.quarantined = accepted.quarantined;
                }
                BrokerEvent::NativeLifetimeRecorded { operation, state } => {
                    state
                        .validate_platform(&broker_header.ok_or_else(invalid)?.os)
                        .map_err(|_| invalid())?;
                    let accepted = result
                        .accepted
                        .iter_mut()
                        .find(|a| a.request.helper_operation == *operation)
                        .ok_or_else(invalid)?;
                    if !matches!(
                        accepted.request.action,
                        DesktopAction::Activate {
                            mode: DesktopMode::NativeActivation,
                            ..
                        }
                    ) || (accepted.quarantined && accepted.state.native_lifetime.is_none())
                        || accepted.state.cleanup_complete
                    {
                        return Err(invalid());
                    }
                    if let Some(old) = &accepted.state.native_lifetime {
                        state.validate_progress_from(old).map_err(|_| invalid())?;
                    }
                    accepted.state.native_lifetime = Some(state.clone());
                    if state.primary == NativePrimaryState::Exited {
                        accepted.state.primary = BrokerPrimary::Exited;
                    }
                    if state.local_retired() {
                        accepted.state.owner = BrokerOwner::Settled;
                    }
                    accepted.state.quarantined = accepted.quarantined;
                }
                BrokerEvent::LifetimeRecorded {
                    operation,
                    primary,
                    descendants,
                    cleanup_complete,
                } => {
                    let accepted = result
                        .accepted
                        .iter_mut()
                        .find(|a| a.request.helper_operation == *operation)
                        .ok_or_else(invalid)?;
                    if !matches!(
                        accepted.request.action,
                        DesktopAction::Activate {
                            mode: DesktopMode::OwnedExecutable,
                            ..
                        }
                    ) || accepted.state.owned_lifetime.is_some()
                        || (accepted.state.cleanup_complete && !cleanup_complete)
                        || (accepted.state.primary == BrokerPrimary::Exited
                            && *primary != BrokerPrimary::Exited)
                        || (accepted.state.primary == BrokerPrimary::NotCreated
                            && *primary != BrokerPrimary::NotCreated)
                        || (accepted.state.descendants == BrokerDescendants::Joined
                            && *descendants != BrokerDescendants::Joined)
                        || (matches!(primary, BrokerPrimary::NotCreated)
                            != matches!(descendants, BrokerDescendants::NotCreated))
                        || (*cleanup_complete
                            && (!matches!(
                                primary,
                                BrokerPrimary::Exited | BrokerPrimary::NotCreated
                            ) || !matches!(
                                descendants,
                                BrokerDescendants::Joined | BrokerDescendants::NotCreated
                            )))
                    {
                        return Err(invalid());
                    }
                    // Irreversible observations retain their original meaning;
                    // they do not reconstruct a live native owner on restart.
                    if matches!(primary, BrokerPrimary::Exited | BrokerPrimary::NotCreated) {
                        accepted.state.primary = *primary;
                    }
                    if matches!(
                        descendants,
                        BrokerDescendants::Joined | BrokerDescendants::NotCreated
                    ) {
                        accepted.state.descendants = *descendants;
                    }
                    if *cleanup_complete {
                        accepted.state.owner = BrokerOwner::Settled;
                        accepted.state.primary = *primary;
                        accepted.state.descendants = *descendants;
                        accepted.state.cleanup_complete = true;
                        accepted.state.quarantined = accepted.quarantined;
                    }
                }
                BrokerEvent::Quarantined { operation } => {
                    let accepted = result
                        .accepted
                        .iter_mut()
                        .find(|a| a.request.helper_operation == *operation)
                        .ok_or_else(invalid)?;
                    if accepted.quarantined {
                        return Err(invalid());
                    }
                    accepted.quarantined = true;
                    accepted.state.quarantined = true;
                }
            }
        }
        let lifetimes: Vec<_> = result
            .accepted
            .iter()
            .filter(|a| matches!(a.request.action, DesktopAction::Activate { .. }))
            .map(|a| (a.request.helper_operation.clone(), a.state.clone()))
            .collect();
        for accepted in &mut result.accepted {
            if let DesktopAction::RequestClose {
                original_activation_operation,
                ..
            } = &accepted.request.action
                && let Some((_, lifetime)) = lifetimes
                    .iter()
                    .find(|(op, _)| op == original_activation_operation)
            {
                accepted.state.native_lifetime = lifetime.native_lifetime.clone();
                accepted.state.owned_lifetime = lifetime.owned_lifetime.clone();
                accepted.state.owner = lifetime.owner;
                accepted.state.primary = lifetime.primary;
                accepted.state.descendants = lifetime.descendants;
                accepted.state.cleanup_complete = lifetime.cleanup_complete;
                accepted.state.quarantined = accepted.quarantined || lifetime.quarantined;
            }
        }
        if helper {
            let mut ids = std::collections::BTreeSet::new();
            for id in result
                .binds
                .iter()
                .map(|b| b.0.helper_operation.clone())
                .chain(
                    result
                        .helper_actions
                        .iter()
                        .map(|(r, _)| r.helper_operation.clone()),
                )
                .chain(records.iter().filter_map(|r| match r.entry() {
                    Entry::OperationAccepted { operation, .. } => Some(operation.to_hex()),
                    _ => None,
                }))
            {
                if !ids.insert(id) || ids.len() > super::MAX_OPERATIONS {
                    return Err(LedgerError::Capacity);
                }
            }
        }
        Ok(result)
    }
    pub fn query(&self, request: &DesktopRequest) -> Result<BrokerState, Error> {
        request.validate()?;
        let accepted = self
            .accepted
            .iter()
            .find(|a| a.request.helper_operation == request.helper_operation)
            .ok_or(Error::InvalidValue)?;
        if accepted.request != *request {
            return Err(Error::InvalidValue);
        }
        Ok(accepted.state.clone())
    }
    pub fn original(&self, operation: &str) -> Option<&DesktopRequest> {
        self.accepted
            .iter()
            .find(|a| a.request.helper_operation == operation)
            .map(|a| &a.request)
    }
    pub fn accepted_count(&self) -> usize {
        self.accepted.len()
    }
    pub fn requests(&self) -> impl Iterator<Item = &DesktopRequest> {
        self.accepted.iter().map(|a| &a.request)
    }
    pub fn binding(&self, original: &BrokerBind) -> Result<Option<&BrokerBindingReceipt>, Error> {
        let (stored, _, receipt) = self
            .binds
            .iter()
            .find(|(b, _, _)| b.helper_operation == original.helper_operation)
            .ok_or(Error::InvalidValue)?;
        if stored != original {
            return Err(Error::InvalidValue);
        }
        Ok(receipt.as_ref())
    }
    pub fn binding_original(&self) -> Option<&BrokerBind> {
        self.binds.first().map(|b| &b.0)
    }
    pub fn binding_scope(&self, original: &BrokerBind) -> Option<IntentPublicationScope> {
        self.binds
            .iter()
            .find(|(b, _, _)| b == original)
            .and_then(|b| b.1)
    }
    pub fn helper_request(
        &self,
        identity: &BrokerOperationIdentity,
    ) -> Result<Option<&DesktopRequest>, Error> {
        let found = self
            .helper_actions
            .iter()
            .find(|(r, _)| r.helper_operation == identity.helper_operation);
        match found {
            Some((r, _)) if r.identity()? == *identity => Ok(Some(r)),
            Some(_) => Err(Error::InvalidValue),
            None => Ok(None),
        }
    }
    pub fn helper_state(
        &self,
        identity: &BrokerOperationIdentity,
    ) -> Result<Option<&BrokerState>, Error> {
        self.helper_request(identity)?;
        Ok(self
            .helper_actions
            .iter()
            .find(|(r, _)| r.helper_operation == identity.helper_operation)
            .and_then(|(_, s)| s.as_ref()))
    }
    pub fn helper_released(&self) -> bool {
        self.helper_released
    }
    pub fn is_quarantined(&self, operation: &str) -> bool {
        self.accepted
            .iter()
            .any(|a| a.request.helper_operation == operation && a.quarantined)
    }
}
