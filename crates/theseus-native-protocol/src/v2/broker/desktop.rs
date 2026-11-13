//! Desktop manifest, actions, requests and observations.
use super::*;

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
