//! One original Shell HANDLE and original-thread COM owner; no PID attachment.
use super::native_policy::{advance_lifetime, initial_lifetime};
use std::{io, sync::atomic::AtomicBool, time::Instant};
use theseus_host::{
    desktop::{
        owned::{CommittedReadinessProbe, ExecutableBinding},
        session::{BoundSession, SessionBinding},
        windows as host,
    },
    services::readiness::{LocalChallenge, ReadinessObservation},
};
use theseus_native_protocol::v2::{self, broker::*};
pub struct Prepared {
    pub(crate) session: BoundSession,
    pub(crate) binding: ExecutableBinding,
    entry: host::AdmittedShortcut,
    ready: LocalChallenge,
    close: LocalChallenge,
    pub(crate) lifetime: NativeLifetimeState,
}
pub struct Retained {
    native: host::NativeActivation,
    lifetime: NativeLifetimeState,
}
pub(crate) fn admit(
    tuple: &v2::DesktopDefinition,
    apartment: host::ComApartment,
) -> io::Result<host::AdmittedShortcut> {
    let x = &tuple.executable;
    let d = &tuple.desktop;
    if x.root != d.entry_root || x.working_directory != x.root {
        return Err(invalid("closed shortcut root/cwd differs"));
    }

    let profile = host::ShortcutProfile {
        root: d.entry_root.clone(),
        entry: d.entry.clone(),
        executable: x.relative_path.clone(),
        arguments: x.arguments[1..].to_vec(),
        entry_artifact: host::ArtifactCommitment {
            length: tuple.entry_length,
            sha256: super::launch::digest(&d.entry_digest)?,
        },
        executable_artifact: host::ArtifactCommitment {
            length: x.length,
            sha256: super::launch::digest(&x.sha256)?,
        },
    };
    host::AdmittedShortcut::parse(apartment, host::RetainedShortcut::open(profile)?)
}
pub(crate) fn prepare(
    _config: &super::BrokerServerConfig,
    tuple: &v2::DesktopDefinition,
    session: BoundSession,
    binding: ExecutableBinding,
    entry: host::AdmittedShortcut,
    ready: LocalChallenge,
    close: LocalChallenge,
) -> io::Result<Prepared> {
    let definition = tuple
        .native
        .as_ref()
        .ok_or_else(|| invalid("Windows native definition missing"))?;
    Ok(Prepared {
        session,
        binding,
        entry,
        ready,
        close,
        lifetime: initial_lifetime(definition)?,
    })
}
impl Prepared {
    pub(crate) fn start(
        self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> (Retained, Option<io::Error>) {
        let (native, error) = match host::NativeActivation::begin(
            self.session,
            self.entry,
            self.binding,
            self.ready,
            self.close,
            host::NativeLifetime::OwnedPrimaryExited,
            deadline,
            cancel,
        ) {
            Ok(owner) => (owner, None),
            Err(failure) => (*failure.owner, Some(failure.error)),
        };
        let mut retained = Retained {
            native,
            lifetime: self.lifetime,
        };
        // A returned invocation with no HANDLE is observed Unavailable primary,
        // not a later loss of previously owned HANDLE authority.
        let projection = retained.refresh();
        (retained, error.or_else(|| projection.err()))
    }
    pub(crate) fn cleanup(&mut self) -> io::Result<bool> {
        self.ready.cleanup()?;
        self.close.cleanup()?;
        Ok(true)
    }
}
impl Retained {
    pub(crate) fn errors_truncated(&self) -> bool {
        self.native.errors_truncated()
    }
    pub(crate) fn state(&self) -> NativeLifetimeState {
        self.lifetime.clone()
    }
    pub(crate) fn revoked(&self) -> bool {
        self.native.observation().session_revoked
    }
    pub(crate) fn errors(&self) -> &[io::Error] {
        self.native.retained_errors()
    }
    pub(crate) fn current_ready_probe(&self) -> io::Result<&CommittedReadinessProbe> {
        self.native
            .current_ready_probe()
            .ok_or_else(|| invalid("native IPC owner retired"))
    }
    pub(crate) fn begin_fresh_ready_probe(
        &mut self,
        next: CommittedReadinessProbe,
    ) -> io::Result<()> {
        self.native.begin_fresh_ready_probe(next).map(|_| ())
    }
    pub(crate) fn poll_ready(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<ReadinessObservation>> {
        let result = self.native.poll_ready(deadline, cancel)?;
        self.refresh()?;
        Ok(result)
    }
    pub(crate) fn poll_request_close(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<ReadinessObservation>> {
        let result = self.native.poll_request_close(deadline, cancel)?;
        self.refresh()?;
        Ok(result)
    }
    fn refresh(&mut self) -> io::Result<()> {
        let observed = self.native.observation();
        let mut next = self.lifetime.clone();
        if observed.primary_exited {
            next.primary = NativePrimaryState::Exited;
            next.instance = NativeInstanceState::Terminated;
        } else if observed.returned_original_process {
            next.primary = NativePrimaryState::Running;
            if observed.fresh_instance_authenticated {
                next.instance = NativeInstanceState::Live;
            }
        } else if observed.effect == host::ActivationEffect::Unavailable {
            next.primary = NativePrimaryState::Unavailable;
            next.instance = NativeInstanceState::Unknown;
        }
        if observed.effect == host::ActivationEffect::Unknown
            && next.instance != NativeInstanceState::Terminated
        {
            next.instance = NativeInstanceState::Unknown;
        }
        if self.lifetime.instance == NativeInstanceState::Unknown {
            next.instance = NativeInstanceState::Unknown;
        }
        if self.lifetime.primary == NativePrimaryState::Unknown {
            next.primary = NativePrimaryState::Unknown;
        }
        if observed.cleanup_complete {
            next.local_resources = NativeLocalState::Retired;
        }
        self.lifetime = advance_lifetime(&self.lifetime, next)?;
        Ok(())
    }
    pub(crate) fn observe_primary(&mut self) -> io::Result<bool> {
        let exited = self.native.observe_primary()?;
        self.refresh()?;
        Ok(exited)
    }
    pub(crate) fn observe_session(&mut self, observed: Option<&SessionBinding>) -> io::Result<()> {
        self.native.observe_session(observed)
    }
    pub(crate) fn cleanup(&mut self) -> io::Result<bool> {
        let done = self.native.try_cleanup()?;
        self.refresh()?;
        Ok(done)
    }
    pub(crate) fn mark_uncertain(&mut self) {
        if !matches!(
            self.lifetime.primary,
            NativePrimaryState::Exited
                | NativePrimaryState::Unavailable
                | NativePrimaryState::NotCreated
        ) {
            self.lifetime.primary = NativePrimaryState::Unknown;
        }
        if !matches!(
            self.lifetime.instance,
            NativeInstanceState::Terminated | NativeInstanceState::NotCreated
        ) {
            self.lifetime.instance = NativeInstanceState::Unknown;
        }
    }
    pub(crate) fn pump(&mut self) -> io::Result<()> {
        self.native.pump_once()
    }
}
fn invalid(s: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, s)
}
