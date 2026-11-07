//! Closed original-owner dispatch shared by the existing broker runtime.
use super::*;
#[cfg(target_os = "linux")]
use crate::broker_transport::native_linux;
#[cfg(target_os = "macos")]
use crate::broker_transport::native_macos;
#[cfg(windows)]
use crate::broker_transport::native_windows;
use theseus_host::desktop::owned::CommittedReadinessProbe;
pub enum PreparedActivation {
    Owned(Box<OwnedLaunch>),
    #[cfg(target_os = "linux")]
    LinuxGio(Box<native_linux::Prepared>),
    #[cfg(target_os = "macos")]
    Macos(Box<native_macos::Prepared>),
    #[cfg(windows)]
    Windows(Box<native_windows::Prepared>),
}
pub enum RetainedActivation {
    Owned(Box<OwnedExecutable>),
    #[cfg(target_os = "linux")]
    LinuxGio(Box<native_linux::Retained>),
    #[cfg(target_os = "macos")]
    Macos(Box<native_macos::Retained>),
    #[cfg(windows)]
    Windows(Box<native_windows::Retained>),
}
pub(crate) struct ActivationObservation {
    pub evidence: DesktopEvidence,
    pub primary: BrokerPrimary,
    pub descendants: BrokerDescendants,
    pub local_cleanup_complete: bool,
    pub session_revoked: bool,
    pub cleanup_complete: bool,
    pub native_lifetime: Option<NativeLifetimeState>,
}
impl PreparedActivation {
    pub(crate) fn mode(&self) -> DesktopMode {
        match self {
            Self::Owned(_) => DesktopMode::OwnedExecutable,
            _ => DesktopMode::NativeActivation,
        }
    }
    pub(crate) fn binding(&self) -> &ExecutableBinding {
        match self {
            Self::Owned(v) => &v.binding,
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => &v.binding,
            #[cfg(target_os = "macos")]
            Self::Macos(v) => &v.binding,
            #[cfg(windows)]
            Self::Windows(v) => &v.binding,
        }
    }
    pub(crate) fn session(&self) -> &SessionBinding {
        match self {
            Self::Owned(v) => v.session.binding(),
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => &v.session,
            #[cfg(target_os = "macos")]
            Self::Macos(v) => v.session.binding(),
            #[cfg(windows)]
            Self::Windows(v) => v.session.binding(),
        }
    }
    pub(crate) fn initial_lifetime(&self) -> Option<NativeLifetimeState> {
        match self {
            Self::Owned(_) => None,
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => Some(v.lifetime.clone()),
            #[cfg(target_os = "macos")]
            Self::Macos(v) => Some(v.lifetime.clone()),
            #[cfg(windows)]
            Self::Windows(v) => Some(v.lifetime.clone()),
        }
    }
    pub(crate) fn cleanup(&mut self) -> io::Result<bool> {
        match self {
            Self::Owned(v) => {
                v.ready_channel.cleanup()?;
                v.close_channel.cleanup()?;
                Ok(true)
            }
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => v.cleanup(),
            #[cfg(target_os = "macos")]
            Self::Macos(v) => v.cleanup(),
            #[cfg(windows)]
            Self::Windows(v) => v.cleanup(),
        }
    }
    pub(crate) fn start(
        self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> (RetainedActivation, Option<io::Error>) {
        match self {
            Self::Owned(v) => {
                let v = *v;
                #[cfg(target_os = "linux")]
                let result = OwnedExecutable::spawn_in_dedicated_broker(
                    v.scope,
                    v.session,
                    &v.command,
                    v.binding,
                    v.ready_channel,
                    v.close_channel,
                );
                #[cfg(not(target_os = "linux"))]
                let result = OwnedExecutable::spawn(
                    v.session,
                    &v.command,
                    v.binding,
                    v.ready_channel,
                    v.close_channel,
                );
                match result {
                    Ok(owner) => (RetainedActivation::Owned(Box::new(owner)), None),
                    Err(failure) => (
                        RetainedActivation::Owned(failure.owner),
                        Some(failure.error),
                    ),
                }
            }
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => {
                let (owner, error) = (*v).start(deadline, cancel);
                (RetainedActivation::LinuxGio(Box::new(owner)), error)
            }
            #[cfg(target_os = "macos")]
            Self::Macos(v) => {
                let _ = (deadline, cancel);
                let (owner, error) = (*v).start();
                (RetainedActivation::Macos(Box::new(owner)), error)
            }
            #[cfg(windows)]
            Self::Windows(v) => {
                let (owner, error) = (*v).start(deadline, cancel);
                (RetainedActivation::Windows(Box::new(owner)), error)
            }
        }
    }
}
impl RetainedActivation {
    pub(crate) fn observation(&self) -> ActivationObservation {
        if let Self::Owned(v) = self {
            let o = v.observation();
            return ActivationObservation {
                evidence: match o.evidence {
                    theseus_host::desktop::owned::ExecutableEvidence::OwnedExecutable => {
                        DesktopEvidence::OwnedExecutable
                    }
                    theseus_host::desktop::owned::ExecutableEvidence::SourceOwnedExecutable => {
                        DesktopEvidence::SourceOwnedExecutable
                    }
                },
                primary: if o.primary_exited {
                    BrokerPrimary::Exited
                } else if o.descendants == DescendantScope::NotCreated {
                    BrokerPrimary::NotCreated
                } else {
                    BrokerPrimary::Running
                },
                descendants: descendants(o.descendants),
                local_cleanup_complete: o.local_cleanup_complete,
                session_revoked: o.session_revoked,
                cleanup_complete: aggregate_cleanup(&o),
                native_lifetime: None,
            };
        }
        let (native, revoked) = match self {
            Self::Owned(_) => unreachable!(),
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => (v.state(), v.revoked()),
            #[cfg(target_os = "macos")]
            Self::Macos(v) => (v.state(), v.revoked()),
            #[cfg(windows)]
            Self::Windows(v) => (v.state(), v.revoked()),
        };
        ActivationObservation {
            evidence: DesktopEvidence::NativeActivation,
            primary: if native.primary == NativePrimaryState::Exited {
                BrokerPrimary::Exited
            } else {
                BrokerPrimary::Unknown
            },
            descendants: BrokerDescendants::Unknown,
            local_cleanup_complete: native.local_retired(),
            session_revoked: revoked,
            cleanup_complete: false,
            native_lifetime: Some(native),
        }
    }
    pub(crate) fn mark_uncertain(&mut self) {
        match self {
            Self::Owned(_) => {}
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => v.mark_uncertain(),
            #[cfg(target_os = "macos")]
            Self::Macos(v) => v.mark_uncertain(),
            #[cfg(windows)]
            Self::Windows(v) => v.mark_uncertain(),
        }
    }
    pub(crate) fn current_ready_probe(&self) -> io::Result<&CommittedReadinessProbe> {
        match self {
            Self::Owned(v) => Ok(v.current_ready_probe()),
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => Ok(v.current_ready_probe()),
            #[cfg(target_os = "macos")]
            Self::Macos(v) => Ok(v.current_ready_probe()),
            #[cfg(windows)]
            Self::Windows(v) => v.current_ready_probe(),
        }
    }
    pub(crate) fn begin_fresh_ready_probe(
        &mut self,
        next: CommittedReadinessProbe,
    ) -> io::Result<()> {
        match self {
            Self::Owned(v) => v.begin_fresh_ready_probe(next).map(|_| ()),
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => v.begin_fresh_ready_probe(next).map(|_| ()),
            #[cfg(target_os = "macos")]
            Self::Macos(v) => v.begin_fresh_ready_probe(next),
            #[cfg(windows)]
            Self::Windows(v) => v.begin_fresh_ready_probe(next),
        }
    }
    pub(crate) fn probe_ready(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<theseus_host::services::readiness::ReadinessObservation> {
        match self {
            Self::Owned(v) => v.probe_ready(deadline, cancel),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native readiness requires original server polling",
            )),
        }
    }
    pub(crate) fn request_close(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<theseus_host::services::readiness::ReadinessObservation> {
        match self {
            Self::Owned(v) => v.request_close(deadline, cancel),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native close requires original server polling",
            )),
        }
    }
    pub(crate) fn try_stdout(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Owned(v) => v.try_stdout(bytes),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native stdout unavailable",
            )),
        }
    }
    pub(crate) fn try_stderr(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Owned(v) => v.try_stderr(bytes),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native stderr unavailable",
            )),
        }
    }
    pub(crate) fn pump(&mut self) -> io::Result<()> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Macos(v) => v.pump(),
            #[cfg(windows)]
            Self::Windows(v) => v.pump(),
            _ => Ok(()),
        }
    }
    pub(crate) fn retained_errors(&self) -> &[io::Error] {
        match self {
            Self::Owned(v) => v.retained_errors(),
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => v.errors(),
            #[cfg(target_os = "macos")]
            Self::Macos(v) => v.errors(),
            #[cfg(windows)]
            Self::Windows(v) => v.errors(),
        }
    }
    pub(crate) fn errors_truncated(&self) -> bool {
        match self {
            Self::Owned(v) => v.errors_truncated(),
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => v.errors_truncated(),
            #[cfg(target_os = "macos")]
            Self::Macos(v) => v.errors_truncated(),
            #[cfg(windows)]
            Self::Windows(v) => v.errors_truncated(),
        }
    }
    pub(crate) fn observe_primary(&mut self) -> io::Result<bool> {
        match self {
            Self::Owned(v) => v.observe_primary(),
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => v.observe_primary(),
            #[cfg(target_os = "macos")]
            Self::Macos(v) => v.observe_primary(),
            #[cfg(windows)]
            Self::Windows(v) => v.observe_primary(),
        }
    }
    pub(crate) fn try_cleanup(&mut self) -> io::Result<bool> {
        match self {
            Self::Owned(v) => v.try_cleanup(),
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => v.cleanup(),
            #[cfg(target_os = "macos")]
            Self::Macos(v) => v.cleanup(),
            #[cfg(windows)]
            Self::Windows(v) => v.cleanup(),
        }
    }
    pub(crate) fn observe_session(&mut self, observed: Option<&SessionBinding>) -> io::Result<()> {
        match self {
            Self::Owned(v) => v.observe_session(observed),
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => v.observe_session(observed),
            #[cfg(target_os = "macos")]
            Self::Macos(v) => v.observe_session(observed),
            #[cfg(windows)]
            Self::Windows(v) => v.observe_session(observed),
        }
    }
    pub(crate) fn poll_ready(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<theseus_host::services::readiness::ReadinessObservation>> {
        match self {
            Self::Owned(v) => v.poll_ready(deadline, cancel),
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => v.poll_ready(deadline, cancel),
            #[cfg(target_os = "macos")]
            Self::Macos(v) => v.poll_ready(deadline, cancel),
            #[cfg(windows)]
            Self::Windows(v) => v.poll_ready(deadline, cancel),
        }
    }
    pub(crate) fn poll_request_close(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<theseus_host::services::readiness::ReadinessObservation>> {
        match self {
            Self::Owned(v) => v.poll_request_close(deadline, cancel),
            #[cfg(target_os = "linux")]
            Self::LinuxGio(v) => v.poll_request_close(deadline, cancel),
            #[cfg(target_os = "macos")]
            Self::Macos(v) => v.poll_request_close(deadline, cancel),
            #[cfg(windows)]
            Self::Windows(v) => v.poll_request_close(deadline, cancel),
        }
    }
}
