//! An immutable supplied session binding. Observing replacement permanently
//! revokes mutation authority; a later matching handshake cannot restore it.
use crate::{
    principal::CurrentPrincipal,
    services::readiness::{NamedPipe, UnixPath},
};
use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopKind {
    Linux,
    MacOs,
    Windows,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopMode {
    NativeActivation,
    OwnedExecutable,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrokerEndpoint {
    Unix(UnixPath),
    Pipe(NamedPipe),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SuppliedDisplay {
    X11 {
        display: String,
        authority_file: String,
        bus: UnixPath,
    },
    Wayland {
        socket: UnixPath,
        runtime_directory: String,
        bus: UnixPath,
    },
    MacOsGui {
        domain: String,
    },
    WindowsInteractive {
        session_id: u32,
        window_station: String,
        desktop: String,
    },
    /// Source tests only. This is never evidence of an interactive session.
    #[cfg(any(test, feature = "test-support", feature = "desktop-test-support"))]
    SourceOwnedHeadless,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionBinding {
    pub target_id: String,
    pub ownership_id: [u8; 16],
    pub provider_epoch: [u8; 32],
    pub session_id: String,
    pub session_epoch: [u8; 32],
    pub broker_header_digest: [u8; 32],
    pub manifest_digest: [u8; 32],
    pub broker_endpoint: BrokerEndpoint,
    pub broker_principal: CurrentPrincipal,
    pub helper_principal: CurrentPrincipal,
    pub display: SuppliedDisplay,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionAuthority {
    pub kind: DesktopKind,
    pub binding: SessionBinding,
    pub allowed_modes: Vec<DesktopMode>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionRefusal {
    InvalidAuthority,
    WrongPlatform,
    WrongBinding,
    ModeDenied,
    SessionZero,
    Revoked,
    NativeActivationUnavailable,
}
impl std::fmt::Display for SessionRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "desktop session refused: {self:?}")
    }
}
impl std::error::Error for SessionRefusal {}
impl From<SessionRefusal> for io::Error {
    fn from(value: SessionRefusal) -> Self {
        Self::new(io::ErrorKind::PermissionDenied, value)
    }
}

/// Constructed only after the caller authenticates a broker handshake against
/// separately supplied authority. It never queries the current display or bus.
#[derive(Debug)]
pub struct BoundSession {
    authority: SessionAuthority,
    revoked: bool,
}
impl BoundSession {
    pub fn admit(
        authority: SessionAuthority,
        observed: &SessionBinding,
    ) -> Result<Self, SessionRefusal> {
        authority.validate()?;
        if &authority.binding != observed {
            return Err(SessionRefusal::WrongBinding);
        }
        Ok(Self {
            authority,
            revoked: false,
        })
    }
    pub fn binding(&self) -> &SessionBinding {
        &self.authority.binding
    }
    pub fn kind(&self) -> DesktopKind {
        self.authority.kind
    }
    pub fn is_revoked(&self) -> bool {
        self.revoked
    }
    pub fn observe(&mut self, observed: Option<&SessionBinding>) -> Result<(), SessionRefusal> {
        if self.revoked {
            return Err(SessionRefusal::Revoked);
        }
        if observed != Some(&self.authority.binding) {
            self.revoked = true;
            return Err(SessionRefusal::WrongBinding);
        }
        Ok(())
    }
    pub fn authorize(&self, mode: DesktopMode) -> Result<(), SessionRefusal> {
        if self.revoked {
            return Err(SessionRefusal::Revoked);
        }
        if !self.authority.allowed_modes.contains(&mode) {
            return Err(SessionRefusal::ModeDenied);
        }
        if mode == DesktopMode::NativeActivation {
            // Native launchers are separate platform adapters; direct execution
            // cannot manufacture their receipt, including in a source fixture.
            return Err(SessionRefusal::NativeActivationUnavailable);
        }
        Ok(())
    }
    /// Admission ceiling for a concrete native platform adapter. This grants no
    /// launcher capability or process right; the adapter must prove those
    /// separately. Generic `authorize(NativeActivation)` stays unavailable.
    pub(crate) fn authorize_native_adapter(&self, kind: DesktopKind) -> Result<(), SessionRefusal> {
        if self.revoked {
            return Err(SessionRefusal::Revoked);
        }
        if self.authority.kind != kind || self.is_source_owned_headless() {
            return Err(SessionRefusal::WrongPlatform);
        }
        if !self
            .authority
            .allowed_modes
            .contains(&DesktopMode::NativeActivation)
        {
            return Err(SessionRefusal::ModeDenied);
        }
        Ok(())
    }
    pub fn is_source_owned_headless(&self) -> bool {
        #[cfg(any(test, feature = "test-support", feature = "desktop-test-support"))]
        {
            matches!(self.binding().display, SuppliedDisplay::SourceOwnedHeadless)
        }
        #[cfg(not(any(test, feature = "test-support", feature = "desktop-test-support")))]
        {
            false
        }
    }
}
fn identifier(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
fn absolute(v: &str) -> bool {
    v.starts_with('/')
        && v.len() <= 1024
        && !v.as_bytes().contains(&0)
        && !v.split('/').any(|s| matches!(s, "." | ".."))
}
impl SessionAuthority {
    pub fn validate(&self) -> Result<(), SessionRefusal> {
        let b = &self.binding;
        if !identifier(&b.target_id)
            || !identifier(&b.session_id)
            || b.ownership_id == [0; 16]
            || b.provider_epoch == [0; 32]
            || b.session_epoch == [0; 32]
            || b.broker_header_digest == [0; 32]
            || b.manifest_digest == [0; 32]
            || self.allowed_modes.is_empty()
            || self.allowed_modes.len() > 2
            || (self.allowed_modes.len() == 2 && self.allowed_modes[0] == self.allowed_modes[1])
        {
            return Err(SessionRefusal::InvalidAuthority);
        }
        let principals_ok = match (
            &b.helper_principal,
            &b.broker_principal,
            &b.broker_endpoint,
            self.kind,
        ) {
            (
                CurrentPrincipal::Posix { .. },
                CurrentPrincipal::Posix { .. },
                BrokerEndpoint::Unix(_),
                DesktopKind::Linux | DesktopKind::MacOs,
            ) => true,
            (
                CurrentPrincipal::Windows { token_user_sid: h },
                CurrentPrincipal::Windows { token_user_sid: p },
                BrokerEndpoint::Pipe(_),
                DesktopKind::Windows,
            ) => {
                crate::services::readiness::Sid::new(h.clone()).is_ok()
                    && crate::services::readiness::Sid::new(p.clone()).is_ok()
            }
            _ => false,
        };
        if !principals_ok {
            return Err(SessionRefusal::WrongPlatform);
        }
        let display_ok = match (&b.display, self.kind) {
            (
                SuppliedDisplay::X11 {
                    display,
                    authority_file,
                    ..
                },
                DesktopKind::Linux,
            ) => {
                display.strip_prefix(':').is_some_and(|v| {
                    let mut parts = v.split('.');
                    let number = parts.next().unwrap_or_default();
                    let screen = parts.next();
                    !number.is_empty()
                        && number.bytes().all(|b| b.is_ascii_digit())
                        && screen
                            .is_none_or(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                        && parts.next().is_none()
                }) && display.len() <= 32
                    && absolute(authority_file)
            }
            (
                SuppliedDisplay::Wayland {
                    runtime_directory,
                    socket,
                    ..
                },
                DesktopKind::Linux,
            ) => {
                absolute(runtime_directory)
                    && std::path::Path::new(socket.as_str()).parent()
                        == Some(std::path::Path::new(runtime_directory))
            }
            (SuppliedDisplay::MacOsGui { domain }, DesktopKind::MacOs) => domain
                .strip_prefix("gui/")
                .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())),
            (SuppliedDisplay::WindowsInteractive { session_id: 0, .. }, DesktopKind::Windows) => {
                return Err(SessionRefusal::SessionZero);
            }
            (
                SuppliedDisplay::WindowsInteractive {
                    window_station,
                    desktop,
                    ..
                },
                DesktopKind::Windows,
            ) => window_station == "WinSta0" && desktop == "Default",
            #[cfg(any(test, feature = "test-support", feature = "desktop-test-support"))]
            (SuppliedDisplay::SourceOwnedHeadless, _) => {
                self.allowed_modes == [DesktopMode::OwnedExecutable]
            }
            _ => false,
        };
        if !display_ok {
            return Err(SessionRefusal::WrongPlatform);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
