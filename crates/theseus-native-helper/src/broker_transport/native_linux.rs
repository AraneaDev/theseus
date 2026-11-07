//! Original-child GIO routing. Serialized qualification claims are never authority.
use super::native_policy::{advance_lifetime, initial_lifetime};
use std::{io, sync::atomic::AtomicBool, time::Instant};
use theseus_host::{
    desktop::{
        linux as host,
        owned::{CommittedReadinessProbe, ExecutableBinding, TerminalReadinessProbe},
        session::SessionBinding,
    },
    services::readiness::{LocalChallenge, PeerExpectation, ReadinessObservation},
};
use theseus_native_protocol::v2::{self, broker::*};
/// Created only by this helper after both exact original IPC channels retire.
/// It cannot be decoded, cloned, or constructed from a PID/receipt.
pub struct LinuxReapPermit<'a> {
    original: &'a mut host::NativeActivation,
}
impl LinuxReapPermit<'_> {
    /// The trusted dedicated caller may invoke Host's unsafe final reap on this
    /// exact original owner; this permit supplies only the IPC-retired proof.
    pub fn original(&mut self) -> &mut host::NativeActivation {
        self.original
    }
}
pub type OriginalFinalizer = for<'a> fn(LinuxReapPermit<'a>) -> io::Result<bool>;
pub struct Prepared {
    native: Preparation,
    pub(crate) session: SessionBinding,
    pub(crate) binding: ExecutableBinding,
    ready: LocalChallenge,
    close: LocalChallenge,
    pub(crate) lifetime: NativeLifetimeState,
    finalize: OriginalFinalizer,
}
enum Preparation {
    Original(Box<host::PreparedNativeActivation>),
    Qualified(Box<QualifiedPreparation>),
}
struct QualifiedPreparation {
    broker: host::DedicatedGioBroker,
    entry: host::RetainedEntry,
    manifest: [u8; 32],
    application: host::CompiledArtifactQualification,
}
impl Preparation {
    fn observe_session(&mut self, observed: Option<&SessionBinding>) -> io::Result<()> {
        match self {
            Self::Original(original) => original.observe_session(observed),
            Self::Qualified(original) => original.broker.observe_session(observed),
        }
    }
    fn start(self, deadline: Instant, cancel: &AtomicBool) -> (OriginalOwner, Option<io::Error>) {
        if let Err(error) = start_window(
            deadline,
            Instant::now(),
            cancel.load(std::sync::atomic::Ordering::Acquire),
        ) {
            return (OriginalOwner::Deferred(Box::new(self)), Some(error));
        }
        let prepared = match self {
            Self::Original(original) => Ok(*original),
            Self::Qualified(original) => original.broker.prepare(
                original.entry,
                original.manifest,
                host::NativeLifetimeRequirement::OwnedPrimary,
                Some(original.application),
            ),
        };
        match prepared {
            Err(failure) => {
                let error = io::Error::new(failure.error.kind(), failure.error.to_string());
                (
                    OriginalOwner::PreparationFailed(Box::new(failure)),
                    Some(error),
                )
            }
            Ok(prepared) => {
                if let Err(error) = start_window(
                    deadline,
                    Instant::now(),
                    cancel.load(std::sync::atomic::Ordering::Acquire),
                ) {
                    return (OriginalOwner::Unstarted(Box::new(prepared)), Some(error));
                }
                match prepared.activate() {
                    Ok(owner) => (OriginalOwner::Active(Box::new(owner)), None),
                    Err(failure) => (
                        OriginalOwner::Unstarted(failure.prepared),
                        Some(failure.error),
                    ),
                }
            }
        }
    }
}
enum OriginalOwner {
    Active(Box<host::NativeActivation>),
    Deferred(Box<Preparation>),
    Unstarted(Box<host::PreparedNativeActivation>),
    PreparationFailed(Box<host::NativePreparationFailure>),
}
pub struct Retained {
    native: OriginalOwner,
    session: SessionBinding,
    revoked: bool,
    binding: ExecutableBinding,
    probe: CommittedReadinessProbe,
    used: Vec<[u8; 16]>,
    ready: Option<LocalChallenge>,
    close: Option<LocalChallenge>,
    lifetime: NativeLifetimeState,
    errors: Vec<io::Error>,
    finalize: OriginalFinalizer,
}
impl Prepared {
    /// Accepts an independently authenticated original Host preparation, never
    /// qualification DTOs from config. Stock startup instead uses the opaque
    /// verified-bootstrap factory and defers SDK setup until durable acceptance.
    /// Caller retains responsibility for the exact original SDK/graph provenance.
    pub fn from_original(
        native: host::PreparedNativeActivation,
        session: SessionBinding,
        binding: ExecutableBinding,
        definition: &v2::NativeDesktopDefinition,
        ready: LocalChallenge,
        close: LocalChallenge,
        finalize: OriginalFinalizer,
    ) -> io::Result<Self> {
        if !matches!(definition, v2::NativeDesktopDefinition::LinuxGio { .. }) {
            return Err(invalid("wrong original Linux platform"));
        }
        binding.validate()?;
        Ok(Self {
            native: Preparation::Original(Box::new(native)),
            session,
            binding,
            ready,
            close,
            lifetime: initial_lifetime(definition)?,
            finalize,
        })
    }

    // Each authority remains explicit; only the factory owns these original values.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn qualified(
        broker: host::DedicatedGioBroker,
        entry: host::RetainedEntry,
        manifest: [u8; 32],
        application: host::CompiledArtifactQualification,
        session: theseus_host::desktop::session::BoundSession,
        binding: ExecutableBinding,
        definition: &v2::NativeDesktopDefinition,
        ready: LocalChallenge,
        close: LocalChallenge,
        finalize: OriginalFinalizer,
    ) -> io::Result<Self> {
        if !matches!(
            definition,
            v2::NativeDesktopDefinition::LinuxGio {
                lifetime: NativeLifetimeRequirement::OwnedPrimaryExited,
                ..
            }
        ) || broker.session_binding() != session.binding()
        {
            return Err(invalid("qualified original Linux profile/session differs"));
        }
        binding.validate()?;
        let lifetime = initial_lifetime(definition)?;
        Ok(Self {
            native: Preparation::Qualified(Box::new(QualifiedPreparation {
                broker,
                entry,
                manifest,
                application,
            })),
            session: session.binding().clone(),
            binding,
            ready,
            close,
            lifetime,
            finalize,
        })
    }
    pub(crate) fn start(
        self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> (Retained, Option<io::Error>) {
        let (native, error) = self.native.start(deadline, cancel);
        let mut lifetime = self.lifetime;
        if matches!(
            &native,
            OriginalOwner::Unstarted(_)
                | OriginalOwner::PreparationFailed(_)
                | OriginalOwner::Deferred(_)
        ) || matches!(&native,OriginalOwner::Active(owner) if !owner.owned_primary_available())
        {
            lifetime.primary = NativePrimaryState::Unknown;
            lifetime.instance = NativeInstanceState::Unknown;
        }
        let probe = CommittedReadinessProbe {
            challenge: self.binding.ready.clone(),
            expectation: self.binding.expectation.clone(),
        };
        let used = vec![probe.challenge.nonce];
        (
            Retained {
                native,
                session: self.session,
                revoked: false,
                binding: self.binding,
                probe,
                used,
                ready: Some(self.ready),
                close: Some(self.close),
                lifetime,
                errors: vec![],
                finalize: self.finalize,
            },
            error,
        )
    }
    pub(crate) fn cleanup(&mut self) -> io::Result<bool> {
        self.ready.cleanup()?;
        self.close.cleanup()?;
        Ok(true)
    }
}
impl Retained {
    pub(crate) fn take_settled_broker(&mut self) -> io::Result<host::DedicatedGioBroker> {
        if self.revoked
            || self.ready.is_some()
            || self.close.is_some()
            || self.lifetime.local_resources != NativeLocalState::Retired
            || !self.errors().is_empty()
            || self.errors_truncated()
        {
            return Err(invalid(
                "original native IPC/local resources remain retained",
            ));
        }
        match &mut self.native {
            OriginalOwner::Active(original) => original.take_settled_broker(),
            _ => Err(invalid("original qualified native owner unavailable")),
        }
    }
    pub(crate) fn errors_truncated(&self) -> bool {
        match &self.native {
            OriginalOwner::Active(n) => n.errors_truncated(),
            OriginalOwner::Unstarted(_)
            | OriginalOwner::PreparationFailed(_)
            | OriginalOwner::Deferred(_) => false,
        }
    }
    pub(crate) fn mark_uncertain(&mut self) {
        if let OriginalOwner::Active(n) = &mut self.native {
            n.quarantine();
        }
        if !matches!(
            self.lifetime.primary,
            NativePrimaryState::Exited | NativePrimaryState::NotCreated
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
    pub(crate) fn state(&self) -> NativeLifetimeState {
        self.lifetime.clone()
    }
    pub(crate) fn revoked(&self) -> bool {
        self.revoked
    }
    pub(crate) fn errors(&self) -> &[io::Error] {
        match &self.native {
            OriginalOwner::Active(n) if !n.retained_errors().is_empty() => n.retained_errors(),
            _ => &self.errors,
        }
    }
    pub(crate) fn current_ready_probe(&self) -> &CommittedReadinessProbe {
        &self.probe
    }
    pub(crate) fn begin_fresh_ready_probe(
        &mut self,
        next: CommittedReadinessProbe,
    ) -> io::Result<TerminalReadinessProbe> {
        if next.challenge == self.probe.challenge
            || next.expectation != self.probe.expectation
            || self.used.len() >= 64
            || self.used.contains(&next.challenge.nonce)
        {
            return Err(invalid("changed/reused native readiness binding"));
        }
        let mut expected = self.probe.challenge.clone();
        expected.nonce = next.challenge.nonce;
        if next.challenge != expected {
            return Err(invalid("fresh probe changed original binding"));
        }
        let observation = self
            .ready
            .as_mut()
            .ok_or_else(|| invalid("ready retired"))?
            .take_terminal_probe()?;
        self.used.push(next.challenge.nonce);
        let probe = std::mem::replace(&mut self.probe, next);
        Ok(TerminalReadinessProbe { probe, observation })
    }
    pub(crate) fn poll_ready(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<ReadinessObservation>> {
        self.poll_probe(false, deadline, cancel)
    }
    pub(crate) fn poll_request_close(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<ReadinessObservation>> {
        self.poll_probe(true, deadline, cancel)
    }
    fn poll_probe(
        &mut self,
        close: bool,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<ReadinessObservation>> {
        if self.revoked {
            return Err(invalid("original native session revoked"));
        }
        let OriginalOwner::Active(native) = &mut self.native else {
            return Err(invalid("original native launch did not begin"));
        };
        let pid = native.original_peer_pid()?;
        let theseus_host::principal::CurrentPrincipal::Posix {
            effective_uid,
            effective_gid,
        } = &self.session.broker_principal
        else {
            return Err(invalid("Linux peer principal absent"));
        };
        let peer = PeerExpectation {
            uid: *effective_uid,
            gid: *effective_gid,
            cooperative_instance: self.binding.cooperative_instance.clone(),
            instance_binding_digest: self.binding.instance_binding_digest,
        };
        let (channel, challenge) = if close {
            (&mut self.close, &self.binding.close)
        } else {
            (&mut self.ready, &self.probe.challenge)
        };
        let result = channel
            .as_mut()
            .ok_or_else(|| invalid("original IPC retired"))?
            .poll_original(
                challenge,
                &self.binding.expectation,
                &peer,
                pid,
                deadline,
                cancel,
            )?;
        self.observe_primary()?;
        if !close
            && result.as_ref().is_some_and(|o| {
                o.state == theseus_host::services::readiness::ReadinessState::Ready
            })
        {
            let mut next = self.lifetime.clone();
            next.instance = NativeInstanceState::Live;
            self.lifetime = advance_lifetime(&self.lifetime, next)?;
        }
        Ok(result)
    }
    pub(crate) fn observe_primary(&mut self) -> io::Result<bool> {
        let OriginalOwner::Active(native) = &mut self.native else {
            return Ok(false);
        };
        let primary = match native.poll_primary()? {
            host::NativePrimaryState::Unavailable => {
                return Err(invalid("original Linux primary authority unavailable"));
            }
            host::NativePrimaryState::Running => NativePrimaryState::Running,
            host::NativePrimaryState::Exited => NativePrimaryState::Exited,
            host::NativePrimaryState::Unknown => NativePrimaryState::Unknown,
        };
        let mut next = self.lifetime.clone();
        next.primary = primary;
        if primary == NativePrimaryState::Exited {
            next.instance = NativeInstanceState::Terminated;
        }
        self.lifetime = advance_lifetime(&self.lifetime, next)?;
        Ok(primary == NativePrimaryState::Exited)
    }
    pub(crate) fn observe_session(&mut self, observed: Option<&SessionBinding>) -> io::Result<()> {
        if observed != Some(&self.session) {
            self.revoked = true;
        }
        let result = match &mut self.native {
            OriginalOwner::Active(n) => n.observe_session(observed),
            OriginalOwner::Unstarted(n) => n.observe_session(observed),
            OriginalOwner::PreparationFailed(n) => n.observe_session(observed),
            OriginalOwner::Deferred(n) => n.observe_session(observed),
        };
        if self.revoked {
            return Err(invalid("native session revoked"));
        }
        result
    }
    pub(crate) fn cleanup(&mut self) -> io::Result<bool> {
        if !self.observe_primary()? {
            return Ok(false);
        }
        for channel in [&mut self.ready, &mut self.close] {
            if let Some(owner) = channel {
                owner.cleanup()?;
                *channel = None;
            }
        }
        // Both exact channels are retired; no path below can authenticate peer PID again.
        let OriginalOwner::Active(native) = &mut self.native else {
            return Ok(false);
        };
        let reaped = (self.finalize)(LinuxReapPermit { original: native })?;
        let local = native.poll_local_retirement()?;
        if reaped && local {
            let mut next = self.lifetime.clone();
            next.local_resources = NativeLocalState::Retired;
            self.lifetime = advance_lifetime(&self.lifetime, next)?;
        }
        Ok(reaped && local)
    }
}
impl Drop for Retained {
    fn drop(&mut self) {
        if !matches!(self.cleanup(), Ok(true)) {
            std::mem::forget(self.ready.take());
            std::mem::forget(self.close.take());
        }
    }
}
fn start_window(deadline: Instant, now: Instant, cancelled: bool) -> io::Result<()> {
    if super::native_policy::native_return_timely(deadline, now, cancelled) {
        return Ok(());
    }
    Err(io::Error::new(
        if cancelled {
            io::ErrorKind::Interrupted
        } else {
            io::ErrorKind::TimedOut
        },
        "original native activation semantic wait ended; exact owner retained",
    ))
}
fn invalid(s: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, s)
}

#[cfg(test)]
mod start_window_tests {
    use super::*;
    #[test]
    fn original_start_window_refuses_expiry_and_cancel_before_each_native_step() {
        let now = Instant::now();
        let deadline = now + std::time::Duration::from_secs(1);
        assert!(start_window(deadline, now, false).is_ok());
        assert_eq!(
            start_window(deadline, deadline, false).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            start_window(deadline, now, true).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }
}
