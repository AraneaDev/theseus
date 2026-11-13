//! A retained direct executable, its original child scope, pipes and two bounded
//! authenticated channels. This supplies no native-launcher or visible-UI proof.
use super::session::{BoundSession, DesktopKind, DesktopMode, SessionBinding, SuppliedDisplay};
use crate::{
    principal::{CurrentPrincipal, current_principal},
    process::{
        ChildEnvironment, HostCleanup, InputMode, NativeCommand, OwnedChild, OwnedSpawnCleanup,
        PipeReader, PipeWriter,
    },
    services::readiness::{
        LocalChallenge, ReadinessChallenge, ReadinessExpectation, ReadinessObservation,
    },
};
use std::{io, process::ExitStatus, sync::atomic::AtomicBool, time::Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutableEvidence {
    OwnedExecutable,
    SourceOwnedExecutable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescendantScope {
    NotCreated,
    Pending,
    Joined,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifetimeObservation {
    pub evidence: ExecutableEvidence,
    pub native_activation_available: bool,
    pub primary_exited: bool,
    pub descendants: DescendantScope,
    pub local_cleanup_complete: bool,
    pub session_revoked: bool,
}

/// Frozen by the durable broker before effect. The close endpoint is a distinct
/// admitted cooperative-close policy using the same bounded readiness codec;
/// its acknowledgement alone never means either the primary or tree has exited.
#[derive(Clone, Debug)]
pub struct ExecutableBinding {
    pub ready: ReadinessChallenge,
    pub close: ReadinessChallenge,
    pub close_original_activation_operation: String,
    pub expectation: ReadinessExpectation,
    pub cooperative_instance: String,
    pub instance_binding_digest: [u8; 32],
}
/// A fresh read-only sample that the caller durably publishes before invoking
/// the transition. It cannot authorize a new activation or replace its binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommittedReadinessProbe {
    pub challenge: ReadinessChallenge,
    pub expectation: ReadinessExpectation,
}
/// A consumed OLD terminal sample, explicitly retaining its original nonce and
/// expected facts. Transitioning never supplies readiness for the next sample.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalReadinessProbe {
    pub probe: CommittedReadinessProbe,
    pub observation: ReadinessObservation,
}
const MAX_READY_PROBES: usize = 64;
impl ExecutableBinding {
    pub fn validate(&self) -> io::Result<()> {
        use crate::services::readiness::encode_challenge;
        encode_challenge(&self.ready)?;
        encode_challenge(&self.close)?;
        if self.close_original_activation_operation != self.ready.start_or_activation_operation
            || self.close.lease != self.ready.lease
            || self.close.slot != self.ready.slot
            || self.close.resource_binding_digest != self.ready.resource_binding_digest
            || self.close.expectation_digest != self.ready.expectation_digest
            || self.close.start_or_activation_operation == self.ready.start_or_activation_operation
            || self.close.action_digest == self.ready.action_digest
            || self.close.action_digest == [0; 32]
            || self.ready.nonce == self.close.nonce
            || self.ready.nonce == [0; 16]
            || self.close.nonce == [0; 16]
            || self.ready.resource_binding_digest == [0; 32]
            || self.ready.action_digest == [0; 32]
            || self.ready.expectation_digest == [0; 32]
            || self.instance_binding_digest == [0; 32]
            || self.cooperative_instance.is_empty()
            || self.cooperative_instance.len() > 256
            || self.cooperative_instance.as_bytes().contains(&0)
        {
            return Err(invalid("missing original activation/close correlation"));
        }
        Ok(())
    }
}

/// A failed launch carries the same still-owned channels, pipes and native
/// cleanup guard. The caller must retain and poll this owner, including when
/// the primary was never created or no activation acknowledgement was received.
pub struct ExecutableStartFailure {
    pub error: io::Error,
    pub owner: Box<OwnedExecutable>,
}
impl std::fmt::Debug for ExecutableStartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutableStartFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for ExecutableStartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for ExecutableStartFailure {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginalOwnedPrimary {
    Running,
    Exited,
    Unknown,
}
/// Borrowed facts from this exact original owner, never deserialized authority.
pub struct OwnedLifetimeProof<'a> {
    original: &'a OwnedExecutable,
}
impl OwnedLifetimeProof<'_> {
    pub fn binding(&self) -> &ExecutableBinding {
        &self.original.binding
    }
    pub fn session(&self) -> &SessionBinding {
        self.original.session.binding()
    }
    pub fn uncertain(&self) -> bool {
        self.original.session.is_revoked()
            || !self.original.errors.is_empty()
            || self.original.errors_truncated
            || self.original.original_peer_pid.is_none()
    }
    pub fn primary(&self) -> OriginalOwnedPrimary {
        if self.original.primary_exited {
            OriginalOwnedPrimary::Exited
        } else if self.uncertain() {
            OriginalOwnedPrimary::Unknown
        } else {
            OriginalOwnedPrimary::Running
        }
    }
    pub fn local_retired(&self) -> bool {
        #[cfg(unix)]
        {
            let original = self.original;
            !self.uncertain()
                && original.observation().local_cleanup_complete
                && original.primary_exited
                && original
                    .original_peer_pid
                    .zip(original.status)
                    .is_some_and(|(pid, status)| {
                        original
                            .primary_retirement
                            .as_ref()
                            .is_some_and(|proof| proof.matches(pid, status))
                    })
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}
pub struct OwnedExecutable {
    #[cfg(target_os = "linux")]
    scope: Option<super::scope::DedicatedBrokerScope>,
    session: BoundSession,
    binding: ExecutableBinding,
    ready_probe: CommittedReadinessProbe,
    used_ready_nonces: Vec<[u8; 16]>,
    child: Option<OwnedChild>,
    original_peer_pid: Option<u32>,
    spawn_cleanup: Option<OwnedSpawnCleanup>,
    stdin: Option<PipeWriter>,
    stdout: Option<PipeReader>,
    stderr: Option<PipeReader>,
    ready_channel: Option<LocalChallenge>,
    close_channel: Option<LocalChallenge>,
    primary_exited: bool,
    descendants: DescendantScope,
    status: Option<ExitStatus>,
    #[cfg(unix)]
    primary_retirement: Option<crate::process::OriginalChildRetirement>,
    errors: Vec<io::Error>,
    errors_truncated: bool,
}
impl std::fmt::Debug for OwnedExecutable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedExecutable")
            .field("observation", &self.observation())
            .finish_non_exhaustive()
    }
}
impl OwnedExecutable {
    /// A macOS process group supplies original primary/group cleanup rights,
    /// never complete original-descendant ownership. Callers must reject a
    /// required owned-tree contract before any launch when this is false.
    pub const fn owned_process_tree_available() -> bool {
        cfg!(any(target_os = "linux", windows))
    }
    /// Admission only. Structural Mac capability never creates an owner or
    /// waives a real Linux scope/Windows job. Retirement needs the borrowed proof.
    pub fn validate_original_primary_backend(session: &BoundSession) -> io::Result<()> {
        if !cfg!(target_os = "macos") || session.kind() != DesktopKind::MacOs {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "original primary-only owned backend is macOS only",
            ));
        }
        session
            .authorize(DesktopMode::OwnedExecutable)
            .map_err(Into::into)
    }
    pub fn owned_lifetime_proof(&self) -> io::Result<OwnedLifetimeProof<'_>> {
        if !cfg!(target_os = "macos") || self.session.kind() != DesktopKind::MacOs {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "original primary-only owned proof is macOS only",
            ));
        }
        Ok(OwnedLifetimeProof { original: self })
    }
    pub fn spawn(
        session: BoundSession,
        command: &NativeCommand,
        binding: ExecutableBinding,
        ready_channel: LocalChallenge,
        close_channel: LocalChallenge,
    ) -> Result<Self, ExecutableStartFailure> {
        Self::spawn_inner(
            session,
            command,
            binding,
            ready_channel,
            close_channel,
            #[cfg(target_os = "linux")]
            None,
        )
    }
    #[cfg(target_os = "linux")]
    pub fn spawn_in_dedicated_broker(
        scope: super::scope::DedicatedBrokerScope,
        session: BoundSession,
        command: &NativeCommand,
        binding: ExecutableBinding,
        ready_channel: LocalChallenge,
        close_channel: LocalChallenge,
    ) -> Result<Self, ExecutableStartFailure> {
        Self::spawn_inner(
            session,
            command,
            binding,
            ready_channel,
            close_channel,
            Some(scope),
        )
    }
    fn spawn_inner(
        session: BoundSession,
        command: &NativeCommand,
        binding: ExecutableBinding,
        ready_channel: LocalChallenge,
        close_channel: LocalChallenge,
        #[cfg(target_os = "linux")] scope: Option<super::scope::DedicatedBrokerScope>,
    ) -> Result<Self, ExecutableStartFailure> {
        let mut owner = Self {
            #[cfg(target_os = "linux")]
            scope,
            session,
            ready_probe: CommittedReadinessProbe {
                challenge: binding.ready.clone(),
                expectation: binding.expectation.clone(),
            },
            used_ready_nonces: vec![binding.ready.nonce],
            binding,
            child: None,
            original_peer_pid: None,
            spawn_cleanup: None,
            stdin: None,
            stdout: None,
            stderr: None,
            ready_channel: Some(ready_channel),
            close_channel: Some(close_channel),
            primary_exited: false,
            descendants: DescendantScope::NotCreated,
            status: None,
            #[cfg(unix)]
            primary_retirement: None,
            errors: Vec::new(),
            errors_truncated: false,
        };
        let result = (|| {
            owner
                .session
                .authorize(DesktopMode::OwnedExecutable)
                .map_err(io::Error::from)?;
            owner.binding.validate()?;
            validate_command(&owner.session, command)?;
            #[cfg(target_os = "linux")]
            if owner.scope.is_none() {
                return Err(invalid(
                    "Linux desktop lifetime requires an explicit dedicated broker scope",
                ));
            }
            if current_principal()? != owner.session.binding().broker_principal {
                return Err(invalid(
                    "owned executable caller differs from supplied broker principal",
                ));
            }
            if owner.ready_channel.as_ref().unwrap().endpoint()
                == owner.close_channel.as_ref().unwrap().endpoint()
            {
                return Err(invalid(
                    "readiness and close require distinct original endpoints",
                ));
            }
            match OwnedChild::spawn(command, InputMode::Pipe) {
                Ok(acquired) => {
                    owner.descendants = if Self::owned_process_tree_available() {
                        DescendantScope::Pending
                    } else {
                        DescendantScope::Unknown
                    };
                    owner.child = Some(acquired.child);
                    owner.stdin = acquired.stdin;
                    owner.stdout = Some(acquired.stdout);
                    owner.stderr = Some(acquired.stderr);
                    owner.original_peer_pid =
                        Some(owner.child.as_ref().unwrap().original_peer_pid()?);
                    Ok(())
                }
                Err(failure) => {
                    if failure.launch != crate::process::LaunchPhase::NeverCreated {
                        owner.descendants = DescendantScope::Unknown;
                    }
                    for error in failure.cleanup_errors {
                        owner.retain_error(error);
                    }
                    match failure.cleanup {
                        HostCleanup::Complete => {}
                        HostCleanup::Pending(cleanup) => owner.spawn_cleanup = Some(cleanup),
                    }
                    Err(failure.error)
                }
            }
        })();
        match result {
            Ok(()) => Ok(owner),
            Err(error) => Err(ExecutableStartFailure {
                error,
                owner: Box::new(owner),
            }),
        }
    }
    /// Transfer the original dedicated scope only after every original child,
    /// pipe and challenge is retired. The old observation remains immutable.
    #[cfg(target_os = "linux")]
    pub fn take_settled_scope_for_next_generation(
        &mut self,
    ) -> io::Result<super::scope::DedicatedBrokerScope> {
        let observation = self.observation();
        if !observation.local_cleanup_complete
            || !observation.primary_exited
            || observation.descendants != DescendantScope::Joined
            || observation.session_revoked
            || !self.errors.is_empty()
            || self.errors_truncated
        {
            return Err(invalid("original generation is not fully retired"));
        }
        let scope = self
            .scope
            .as_mut()
            .ok_or_else(|| invalid("original scope already transferred"))?;
        scope.rearm_after_join()?;
        Ok(self.scope.take().expect("validated original scope"))
    }
    pub fn binding(&self) -> &ExecutableBinding {
        &self.binding
    }
    pub fn current_ready_probe(&self) -> &CommittedReadinessProbe {
        &self.ready_probe
    }
    /// Call only after publishing the new probe's exact expectation and nonce.
    /// A pending/cancelled/unfinished native probe cannot be replaced. Validation
    /// precedes consuming the old terminal context; native settlement failure
    /// retains the old channel/context and never admits the replacement.
    pub fn begin_fresh_ready_probe(
        &mut self,
        next: CommittedReadinessProbe,
    ) -> io::Result<TerminalReadinessProbe> {
        self.session
            .authorize(DesktopMode::OwnedExecutable)
            .map_err(io::Error::from)?;
        validate_fresh_probe(&self.binding, &next, &self.used_ready_nonces)?;
        if self.observe_primary()? {
            return Err(invalid("original executable has exited"));
        }
        let channel = self
            .ready_channel
            .as_mut()
            .ok_or_else(|| invalid("original readiness channel already settled"))?;
        let observation = match channel.take_terminal_probe() {
            Ok(observation) => observation,
            Err(error) => {
                self.retain_error(error);
                return Err(io::Error::other(
                    "original readiness probe is not natively settled; context retained",
                ));
            }
        };
        self.used_ready_nonces.push(next.challenge.nonce);
        let probe = std::mem::replace(&mut self.ready_probe, next);
        Ok(TerminalReadinessProbe { probe, observation })
    }
    pub fn observe_session(&mut self, observed: Option<&SessionBinding>) -> io::Result<()> {
        self.session.observe(observed).map_err(Into::into)
    }
    pub fn retained_errors(&self) -> &[io::Error] {
        &self.errors
    }
    pub fn errors_truncated(&self) -> bool {
        self.errors_truncated
    }
    fn retain_error(&mut self, error: io::Error) {
        if self.errors.len() < 16 {
            self.errors.push(error)
        } else {
            self.errors_truncated = true
        }
    }
    pub fn observation(&self) -> LifetimeObservation {
        LifetimeObservation {
            evidence: if self.session.is_source_owned_headless() {
                ExecutableEvidence::SourceOwnedExecutable
            } else {
                ExecutableEvidence::OwnedExecutable
            },
            native_activation_available: false,
            primary_exited: self.primary_exited,
            descendants: self.descendants,
            local_cleanup_complete: self.child.is_none()
                && self.spawn_cleanup.is_none()
                && self.stdin.is_none()
                && self.stdout.is_none()
                && self.stderr.is_none()
                && self.ready_channel.is_none()
                && self.close_channel.is_none(),
            session_revoked: self.session.is_revoked(),
        }
    }
    /// Only the held original process object supplies this correlation metadata.
    /// This method never reconstructs an owner from the returned numeric value.
    pub fn original_peer_pid(&self) -> Option<u32> {
        self.original_peer_pid
    }
    pub fn probe_ready(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<ReadinessObservation> {
        self.probe(false, deadline, cancel)
    }
    pub fn request_close(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<ReadinessObservation> {
        self.probe(true, deadline, cancel)
    }
    pub fn poll_ready(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<ReadinessObservation>> {
        self.probe_mode(false, deadline, cancel, true)
    }
    pub fn poll_request_close(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<ReadinessObservation>> {
        self.probe_mode(true, deadline, cancel, true)
    }
    fn probe(
        &mut self,
        close: bool,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<ReadinessObservation> {
        self.probe_mode(close, deadline, cancel, false)?
            .ok_or_else(|| invalid("blocking executable probe unexpectedly yielded"))
    }
    fn probe_mode(
        &mut self,
        close: bool,
        deadline: Instant,
        cancel: &AtomicBool,
        poll_only: bool,
    ) -> io::Result<Option<ReadinessObservation>> {
        self.session
            .authorize(DesktopMode::OwnedExecutable)
            .map_err(io::Error::from)?;
        let pid = self
            .original_peer_pid
            .ok_or_else(|| invalid("original executable was not created"))?;
        let child = self
            .child
            .as_ref()
            .ok_or_else(|| invalid("original executable owner has already settled"))?;
        if self.primary_exited || child.original_peer_pid()? != pid {
            return Err(invalid(
                "original executable peer identity no longer live/reserved",
            ));
        }
        let channel = if close {
            &mut self.close_channel
        } else {
            &mut self.ready_channel
        };
        let channel = channel
            .as_mut()
            .ok_or_else(|| invalid("original channel already settled"))?;
        let (challenge, expectation) = if close {
            (&self.binding.close, &self.binding.expectation)
        } else {
            (&self.ready_probe.challenge, &self.ready_probe.expectation)
        };
        #[cfg(unix)]
        {
            let CurrentPrincipal::Posix {
                effective_uid,
                effective_gid,
            } = self.session.binding().broker_principal
            else {
                return Err(invalid("non-POSIX supplied executable principal"));
            };
            let peer = crate::services::readiness::PeerExpectation {
                uid: effective_uid,
                gid: effective_gid,
                cooperative_instance: self.binding.cooperative_instance.clone(),
                instance_binding_digest: self.binding.instance_binding_digest,
            };
            if poll_only {
                channel.poll_original(challenge, expectation, &peer, pid, deadline, cancel)
            } else {
                channel
                    .probe_original(challenge, expectation, &peer, pid, deadline, cancel)
                    .map(Some)
            }
        }
        #[cfg(windows)]
        {
            let CurrentPrincipal::Windows { ref token_user_sid } =
                self.session.binding().broker_principal
            else {
                return Err(invalid("non-Windows supplied executable principal"));
            };
            let peer = crate::services::readiness::PipePeerExpectation {
                sid: crate::services::readiness::Sid::new(token_user_sid.clone())?,
                original_pid: pid,
                cooperative_instance: self.binding.cooperative_instance.clone(),
                instance_binding_digest: self.binding.instance_binding_digest,
            };
            if poll_only {
                channel.poll(challenge, expectation, &peer, deadline, cancel)
            } else {
                channel
                    .probe(challenge, expectation, &peer, deadline, cancel)
                    .map(Some)
            }
        }
    }
    /// Drains a bounded caller buffer through the existing interruptible pipes.
    /// The caller's existing worker/pump owns scheduling and output budgets.
    #[cfg(test)]
    pub fn try_stdout(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.stdout
            .as_mut()
            .ok_or_else(|| invalid("stdout already settled"))?
            .try_read(out)
    }
    pub fn observe_primary(&mut self) -> io::Result<bool> {
        if self.primary_exited {
            return Ok(true);
        }
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| invalid("original executable was not created"))?;
        match child.exited_reserved() {
            Ok(exited) => {
                self.primary_exited = exited;
                Ok(exited)
            }
            Err(error) => {
                self.descendants = DescendantScope::Unknown;
                self.retain_error(error);
                Err(io::Error::other(
                    "original primary observation failed; native error retained",
                ))
            }
        }
    }
    /// Force cleanup is limited to this original Host child/group/job; session
    /// revocation does not grant access to any replacement broker or process.
    /// False and errors retain every unresolved owner for the existing worker.
    pub fn try_cleanup(&mut self) -> io::Result<bool> {
        let mut failed = false;
        if let Some(writer) = &mut self.stdin {
            match writer.close() {
                Ok(()) => self.stdin = None,
                Err(e) => {
                    self.retain_error(e);
                    failed = true
                }
            }
        }
        for stderr in [false, true] {
            let endpoint = if stderr {
                &mut self.stderr
            } else {
                &mut self.stdout
            };
            if let Some(reader) = endpoint {
                match reader.cancel_and_complete() {
                    Ok(()) => *endpoint = None,
                    Err(e) => {
                        self.retain_error(e);
                        failed = true
                    }
                }
            }
        }
        if let Some(child) = &mut self.child {
            #[cfg(target_os = "linux")]
            let joined = match &mut self.scope {
                Some(scope) => scope.try_join(child),
                None => Err(invalid("original dedicated broker scope unavailable")),
            };
            #[cfg(windows)]
            let joined = child.try_join_original_scope();
            // Preserve legacy original-primary/group cleanup on macOS, without
            // upgrading it into a full descendant-tree receipt.
            #[cfg(target_os = "macos")]
            let joined = child.try_reap_and_release();
            #[cfg(target_os = "linux")]
            if let Some(status) = self
                .scope
                .as_ref()
                .and_then(|scope| scope.primary_exit_status())
            {
                self.primary_exited = true;
                self.status = Some(status);
            }
            match joined {
                Ok(Some(status)) => {
                    #[cfg(unix)]
                    {
                        self.primary_retirement = child.retirement_witness();
                    }
                    self.status = Some(status);
                    self.primary_exited = true;
                    self.child = None;
                    self.descendants = if Self::owned_process_tree_available() {
                        DescendantScope::Joined
                    } else {
                        DescendantScope::Unknown
                    };
                }
                Ok(None) => {}
                Err(e) => {
                    self.descendants = DescendantScope::Unknown;
                    self.retain_error(e);
                    failed = true
                }
            }
        }
        if let Some(cleanup) = &mut self.spawn_cleanup {
            match cleanup.try_complete() {
                Ok(true) => self.spawn_cleanup = None,
                Ok(false) => {}
                Err(e) => {
                    self.retain_error(e);
                    failed = true
                }
            }
        }
        for close in [false, true] {
            let channel = if close {
                &mut self.close_channel
            } else {
                &mut self.ready_channel
            };
            if let Some(ipc) = channel {
                match ipc.cleanup() {
                    Ok(()) => *channel = None,
                    Err(e) => {
                        self.retain_error(e);
                        failed = true
                    }
                }
            }
        }
        if failed {
            return Err(io::Error::other(
                "original desktop cleanup remains retained; see retained_errors",
            ));
        }
        Ok(self.observation().local_cleanup_complete)
    }
}
impl Drop for OwnedExecutable {
    fn drop(&mut self) {
        if !matches!(self.try_cleanup(), Ok(true)) {
            // A bounded quarantine of the original owners is safer than freeing
            // pending kernel operation storage or losing unresolved tree rights.
            std::mem::forget(self.child.take());
            // The Linux scope token contains accounting only and has no Drop
            // effect. Its normal destruction cannot undo the dedicated broker's
            // process-lifetime subreaper state or certify descendant completion.
            std::mem::forget(self.spawn_cleanup.take());
            std::mem::forget(self.stdin.take());
            std::mem::forget(self.stdout.take());
            std::mem::forget(self.stderr.take());
            std::mem::forget(self.ready_channel.take());
            std::mem::forget(self.close_channel.take());
            std::mem::forget(std::mem::take(&mut self.errors));
        }
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn validate_fresh_probe(
    original: &ExecutableBinding,
    next: &CommittedReadinessProbe,
    used: &[[u8; 16]],
) -> io::Result<()> {
    crate::services::readiness::encode_challenge(&next.challenge)?;
    let c = &next.challenge;
    let old = &original.ready;
    if c.protocol != old.protocol
        || c.lease != old.lease
        || c.slot != old.slot
        || c.resource_binding_digest != old.resource_binding_digest
        || c.start_or_activation_operation != old.start_or_activation_operation
        || c.action_digest != old.action_digest
        || c.expectation_digest == [0; 32]
        || c.nonce == [0; 16]
        || c.nonce == original.close.nonce
        || used.contains(&c.nonce)
        || used.len() >= MAX_READY_PROBES
        || next.expectation.version_literal.is_empty()
        || next.expectation.version_literal.len() > 256
        || next.expectation.version_literal.as_bytes().contains(&0)
    {
        return Err(invalid(
            "fresh readiness must preserve original activation and use a new committed bounded probe",
        ));
    }
    Ok(())
}
fn validate_command(session: &BoundSession, command: &NativeCommand) -> io::Result<()> {
    if !std::path::Path::new(&command.program).is_absolute() {
        return Err(invalid(
            "desktop executable must be an explicitly admitted absolute path",
        ));
    }
    let expected = if cfg!(target_os = "linux") {
        DesktopKind::Linux
    } else if cfg!(target_os = "macos") {
        DesktopKind::MacOs
    } else {
        DesktopKind::Windows
    };
    if session.kind() != expected {
        return Err(invalid("desktop session platform differs from native host"));
    }
    let ChildEnvironment::Replace(entries) = &command.environment else {
        return Err(invalid(
            "desktop executable requires an explicit replacement environment",
        ));
    };
    let env = |key: &str| -> io::Result<Option<&std::ffi::OsStr>> {
        let mut found = entries.iter().filter(|(k, _)| k == key);
        let value = found.next().map(|(_, v)| v.as_os_str());
        if found.next().is_some() {
            return Err(invalid("duplicate desktop environment material"));
        }
        Ok(value)
    };
    let equal = |key: &str, value: &str| -> io::Result<bool> {
        Ok(env(key)? == Some(std::ffi::OsStr::new(value)))
    };
    let ok = match &session.binding().display {
        SuppliedDisplay::X11 {
            display,
            authority_file,
            bus,
        } => {
            equal("DISPLAY", display)?
                && equal("XAUTHORITY", authority_file)?
                && equal(
                    "DBUS_SESSION_BUS_ADDRESS",
                    &format!("unix:path={}", bus.as_str()),
                )?
                && env("WAYLAND_DISPLAY")?.is_none()
        }
        SuppliedDisplay::Wayland {
            socket,
            runtime_directory,
            bus,
        } => {
            equal("WAYLAND_DISPLAY", socket.as_str())?
                && equal("XDG_RUNTIME_DIR", runtime_directory)?
                && equal(
                    "DBUS_SESSION_BUS_ADDRESS",
                    &format!("unix:path={}", bus.as_str()),
                )?
                && env("DISPLAY")?.is_none()
                && env("XAUTHORITY")?.is_none()
        }
        _ => [
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "DBUS_SESSION_BUS_ADDRESS",
            "XAUTHORITY",
        ]
        .iter()
        .all(|key| matches!(env(key), Ok(None))),
    };
    if !ok {
        return Err(invalid(
            "command display material differs from original supplied session",
        ));
    }
    command.validate()
}
#[cfg(test)]
mod tests;
