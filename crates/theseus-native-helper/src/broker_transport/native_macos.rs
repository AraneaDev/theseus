//! One retained original LaunchServices manager instance, on the server thread.
use super::native_policy::{advance_lifetime, initial_lifetime};
use std::{io, sync::atomic::AtomicBool, time::Instant};
use theseus_host::{
    desktop::{
        macos as host,
        owned::{CommittedReadinessProbe, ExecutableBinding},
        session::{BoundSession, SessionBinding},
    },
    services::readiness::{LocalChallenge, ReadinessObservation},
};
use theseus_native_protocol::v2::{self, broker::*};
pub struct Prepared {
    pub(crate) session: BoundSession,
    pub(crate) binding: ExecutableBinding,
    entry: host::AdmittedBundle,
    security_session: u32,
    ready: LocalChallenge,
    close: LocalChallenge,
    pub(crate) lifetime: NativeLifetimeState,
}
pub struct Retained {
    native: host::NativeActivation,
    lifetime: NativeLifetimeState,
}
pub(crate) fn prepare(
    config: &super::BrokerServerConfig,
    tuple: &v2::DesktopDefinition,
    session: BoundSession,
    binding: ExecutableBinding,
    entry: host::AdmittedBundle,
    ready: LocalChallenge,
    close: LocalChallenge,
) -> io::Result<Prepared> {
    let definition = tuple
        .native
        .as_ref()
        .ok_or_else(|| invalid("Mac native definition missing"))?;
    let lifetime = initial_lifetime(definition)?;
    let security_session = config
        .header
        .macos_security_session_id
        .filter(|id| *id != 0)
        .ok_or_else(|| invalid("original Mac security session absent"))?;
    Ok(Prepared {
        session,
        binding,
        entry,
        security_session,
        ready,
        close,
        lifetime,
    })
}
impl Prepared {
    pub(crate) fn start(self) -> (Retained, Option<io::Error>) {
        let (native, error) = match host::NativeActivation::start(
            self.entry,
            self.session,
            self.security_session,
            self.binding,
            self.ready,
            self.close,
            host::NativeLifetime::ManagerInstanceTerminated,
        ) {
            Ok(owner) => (owner, None),
            Err(failure) => (*failure.owner, Some(failure.error)),
        };
        let mut retained = Retained {
            native,
            lifetime: self.lifetime,
        };
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
        self.native.session_revoked()
    }
    pub(crate) fn errors(&self) -> &[io::Error] {
        self.native.retained_errors()
    }
    pub(crate) fn current_ready_probe(&self) -> &CommittedReadinessProbe {
        self.native.current_ready_probe()
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
        let mut next = self.lifetime.clone();
        next.instance = if self.native.manager_instance_terminated() {
            NativeInstanceState::Terminated
        } else {
            match self.native.effect() {
                host::ActivationEffect::Acknowledged => NativeInstanceState::Live,
                host::ActivationEffect::Unknown => NativeInstanceState::Unknown,
                _ => next.instance,
            }
        };
        if self.lifetime.instance == NativeInstanceState::Unknown {
            next.instance = NativeInstanceState::Unknown;
        }
        if self.native.local_cleanup_complete() {
            next.local_resources = NativeLocalState::Retired;
        }
        self.lifetime = advance_lifetime(&self.lifetime, next)?;
        Ok(())
    }
    pub(crate) fn observe_primary(&mut self) -> io::Result<bool> {
        // The server's existing cadence advances the retained snapshot once.
        // A scheduling quantum is never an activation deadline.
        self.refresh()?;
        Ok(false)
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
            self.lifetime.instance,
            NativeInstanceState::Terminated | NativeInstanceState::NotCreated
        ) {
            self.lifetime.instance = NativeInstanceState::Unknown;
        }
    }
    pub(crate) fn pump(&mut self) -> io::Result<()> {
        self.native.poll_native_turn()?;
        self.refresh()
    }
}
fn invalid(s: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, s)
}
