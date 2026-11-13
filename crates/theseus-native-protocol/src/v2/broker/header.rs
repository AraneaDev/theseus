//! Path/endpoint validation, broker header, principal, supplied display and authority.
use super::*;

pub(super) fn digest<T: Serialize>(domain: &[u8], value: &T) -> Result<Digest, Error> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::InvalidValue)?;
    if bytes.len() > crate::v2::MAX_BODY_BYTES {
        return Err(Error::LimitExceeded);
    }
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(bytes);
    Ok(Digest::from_bytes(hash.finalize().into()))
}
pub(super) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
pub(super) fn absolute(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 1024
        && !value.contains('\0')
        && !value.split('/').any(|s| matches!(s, "." | ".."))
}
pub(super) fn windows_absolute(value: &str) -> bool {
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
    pub linux_gio_qualification: Option<crate::v2::LinuxGioQualificationRef>,
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
