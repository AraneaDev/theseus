//! Concrete AppKit bridge boundary. No native calls run on construction/import
//! of this module. Caller owns the already supplied broker's main event loop.
use super::{ActivationEffect, ActivationProgress, AdmittedBundle, CallbackResult, invalid};
use crate::{
    desktop::{
        owned::{CommittedReadinessProbe, ExecutableBinding, TerminalReadinessProbe},
        session::{BoundSession, DesktopKind, SuppliedDisplay},
    },
    principal::{CurrentPrincipal, current_principal},
    services::readiness::{LocalChallenge, PeerExpectation, ReadinessObservation, ReadinessState},
};
use std::{
    ffi::{CString, c_char, c_void},
    io,
    marker::PhantomData,
    ptr::NonNull,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
/// Validate the supplied native broker session before acquiring a broker lease.
/// This is the same read-only native session check used before activation.
pub fn validate_supplied_session(
    session: &BoundSession,
    security_session_id: u32,
) -> io::Result<()> {
    validate_supplied_session_uid(session, security_session_id).map(|_| ())
}
fn validate_supplied_session_uid(
    session: &BoundSession,
    security_session_id: u32,
) -> io::Result<u32> {
    session
        .authorize_native_adapter(DesktopKind::MacOs)
        .map_err(io::Error::from)?;
    if current_principal()? != session.binding().broker_principal {
        return Err(invalid(
            "native caller differs from supplied broker principal",
        ));
    }
    let CurrentPrincipal::Posix { effective_uid, .. } = session.binding().broker_principal else {
        return Err(invalid("native macOS broker requires POSIX principal"));
    };
    let SuppliedDisplay::MacOsGui { ref domain } = session.binding().display else {
        return Err(invalid("explicit Aqua domain required"));
    };
    if domain != &format!("gui/{effective_uid}")
        || unsafe { theseus_mac_session_check(security_session_id, effective_uid) } != 1
    {
        return Err(invalid(
            "broker is outside the explicitly supplied graphical session",
        ));
    }
    Ok(effective_uid)
}

#[repr(C)]
#[derive(Default)]
struct Snapshot {
    status: i32,
    original_pid: i32,
    terminated: u8,
    has_application: u8,
    uncertain_pending: u8,
    reserved: u8,
    error_code: i64,
}
unsafe extern "C" {
    fn theseus_mac_session_check(session: u32, uid: u32) -> i32;
    fn theseus_mac_begin(
        bundle: *const c_char,
        executable: *const c_char,
        identifier: *const c_char,
        arguments: *const *const c_char,
        count: usize,
        session: u32,
        uid: u32,
        owner: *mut *mut c_void,
    ) -> i32;
    fn theseus_mac_snapshot(owner: *mut c_void, snapshot: *mut Snapshot) -> i32;
    fn theseus_mac_pump_once() -> i32;
    fn theseus_mac_release_settled(owner: *mut c_void) -> i32;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeLifetime {
    ManagerInstanceTerminated,
    OwnedPrimaryExited,
    OwnedProcessTreeSettled,
}
struct Resources {
    session: BoundSession,
    security_session_id: u32,
    bundle: AdmittedBundle,
    binding: ExecutableBinding,
    ready: LocalChallenge,
    close: LocalChallenge,
    native: Option<NonNull<c_void>>,
}
/// !Send/!Sync: all Rust-side observations, autorelease/event-loop entry and
/// final releases stay on the original supplied broker main thread.
pub struct NativeActivation {
    resources: super::retention::NativeRetention<Resources>,
    progress: ActivationProgress,
    last_snapshot: Snapshot,
    ready_probe: CommittedReadinessProbe,
    used_ready_nonces: Vec<[u8; 16]>,
    terminal_callback_recorded: bool,
    local_settled: bool,
    ready_deadline: Option<Instant>,
    close_deadline: Option<Instant>,
    session_invalid: bool,
    errors: Vec<io::Error>,
    errors_truncated: bool,
    _main_thread: PhantomData<Rc<()>>,
}
pub struct NativeStartFailure {
    pub error: io::Error,
    /// Always preserves the original channels, admitted files and any pending
    /// native callback/configuration/instance, including after launch failure.
    pub owner: Box<NativeActivation>,
}
impl std::fmt::Debug for NativeStartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeStartFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for NativeStartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for NativeStartFailure {}
fn cstring(value: &str) -> io::Result<CString> {
    CString::new(value).map_err(|_| invalid("native launch string contains NUL"))
}
/// Advance only the already supplied broker's original AppKit main run loop.
/// The bridge rejects non-main callers and retains its autorelease/callback state.
pub fn pump_once() -> io::Result<()> {
    if unsafe { theseus_mac_pump_once() } != 1 {
        return Err(io::Error::other(
            "native broker event loop failed or caller is not the original main thread",
        ));
    }
    Ok(())
}
#[derive(Debug)]
struct SharedNativeError(Arc<io::Error>);
impl std::fmt::Display for SharedNativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.0.as_ref(), f)
    }
}
impl std::error::Error for SharedNativeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}
impl NativeActivation {
    /// Durable accepted original action, manifest/slot/profile/argv commitment,
    /// exclusive installer namespace and authenticated session handshake MUST
    /// already be published. This never binds/starts a broker or discovers GUI.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        bundle: AdmittedBundle,
        session: BoundSession,
        supplied_security_session_id: u32,
        binding: ExecutableBinding,
        ready: LocalChallenge,
        close: LocalChallenge,
        lifetime: NativeLifetime,
    ) -> Result<Self, NativeStartFailure> {
        let ready_probe = CommittedReadinessProbe {
            challenge: binding.ready.clone(),
            expectation: binding.expectation.clone(),
        };
        let used_ready_nonces = vec![binding.ready.nonce];
        let mut owner = Self {
            ready_probe,
            used_ready_nonces,
            resources: super::retention::NativeRetention::new(Resources {
                session,
                security_session_id: supplied_security_session_id,
                bundle,
                binding,
                ready,
                close,
                native: None,
            }),
            progress: ActivationProgress::pending(),
            last_snapshot: Snapshot::default(),
            terminal_callback_recorded: false,
            local_settled: false,
            ready_deadline: None,
            close_deadline: None,
            session_invalid: false,
            errors: Vec::new(),
            errors_truncated: false,
            _main_thread: PhantomData,
        };
        let result = owner.begin(lifetime);
        match result {
            Ok(()) => Ok(owner),
            Err(error) => Err(NativeStartFailure {
                error,
                owner: Box::new(owner),
            }),
        }
    }
    fn retain_result<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        result.map_err(|error| {
            let kind = error.kind();
            let error = Arc::new(error);
            if self.errors.len() < 16 {
                self.errors
                    .push(io::Error::new(kind, SharedNativeError(error.clone())));
            } else {
                self.errors_truncated = true;
            }
            io::Error::new(kind, SharedNativeError(error))
        })
    }
    pub fn retained_errors(&self) -> &[io::Error] {
        &self.errors
    }
    pub fn errors_truncated(&self) -> bool {
        self.errors_truncated
    }
    pub fn session_revoked(&self) -> bool {
        self.session_invalid || self.resources.session.is_revoked()
    }
    fn validate_session(&mut self) -> io::Result<u32> {
        if self.session_revoked() {
            return Err(invalid("original graphical session authority revoked"));
        }
        let result = self.validate_session_inner();
        if result.is_err() {
            self.session_invalid = true;
        }
        result
    }
    fn validate_session_inner(&self) -> io::Result<u32> {
        validate_supplied_session_uid(&self.resources.session, self.resources.security_session_id)
    }
    fn begin(&mut self, lifetime: NativeLifetime) -> io::Result<()> {
        if lifetime != NativeLifetime::ManagerInstanceTerminated {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native AppKit activation supplies only manager-instance lifetime",
            ));
        }
        let uid = self.validate_session()?;
        self.resources.binding.validate()?;
        self.resources.bundle.revalidate()?;
        let profile = self.resources.bundle.profile();
        if profile.executable_artifact.length != self.resources.binding.expectation.artifact_length
            || profile.executable_artifact.sha256
                != self.resources.binding.expectation.artifact_sha256
            || self.resources.ready.endpoint() == self.resources.close.endpoint()
        {
            return Err(invalid(
                "native artifact/channel differs from accepted binding",
            ));
        }
        super::probe::validate_static_bootstrap(
            &profile.arguments,
            self.resources.ready.endpoint().as_str(),
            self.resources.close.endpoint().as_str(),
        )?;
        let bundle = cstring(
            self.resources
                .bundle
                .retained
                .bundle_path()
                .to_str()
                .ok_or_else(|| invalid("bundle path is not UTF-8"))?,
        )?;
        let executable = cstring(
            self.resources
                .bundle
                .retained
                .executable_path()
                .to_str()
                .ok_or_else(|| invalid("executable path is not UTF-8"))?,
        )?;
        let identifier = cstring(&profile.bundle_identifier)?;
        let arguments: Vec<_> = profile
            .arguments
            .iter()
            .map(|arg| cstring(arg))
            .collect::<io::Result<_>>()?;
        let pointers: Vec<_> = arguments.iter().map(|arg| arg.as_ptr()).collect();
        let mut raw = std::ptr::null_mut();
        let status = unsafe {
            theseus_mac_begin(
                bundle.as_ptr(),
                executable.as_ptr(),
                identifier.as_ptr(),
                pointers.as_ptr(),
                pointers.len(),
                self.resources.security_session_id,
                uid,
                &mut raw,
            )
        };
        self.resources.native = NonNull::new(raw);
        if status != 1 || self.resources.native.is_none() {
            return Err(io::Error::other(
                "native activation failed/refused; original owner retained",
            ));
        }
        Ok(())
    }
    pub fn effect(&self) -> ActivationEffect {
        self.progress.effect()
    }
    pub fn manager_instance_terminated(&self) -> bool {
        self.progress.manager_instance_terminated()
    }
    pub fn local_cleanup_complete(&self) -> bool {
        self.local_settled
    }
    pub fn binding(&self) -> &ExecutableBinding {
        &self.resources.binding
    }
    pub fn application_ready(&self) -> bool {
        self.progress.application_ready()
    }
    pub fn current_ready_probe(&self) -> &CommittedReadinessProbe {
        &self.ready_probe
    }
    /// Requires independently published next probe. The initial activation nonce
    /// in the original authenticated binding is never rewritten by this method.
    pub fn begin_fresh_ready_probe(
        &mut self,
        next: CommittedReadinessProbe,
    ) -> io::Result<TerminalReadinessProbe> {
        self.validate_session()?;
        super::probe::validate_fresh_probe(
            &self.resources.binding,
            &next,
            &self.used_ready_nonces,
        )?;
        self.poll_native_turn()?;
        if self.last_snapshot.status != 1 || self.last_snapshot.terminated != 0 {
            return Err(invalid(
                "fresh readiness requires the original live manager instance",
            ));
        }
        let observation = self.resources.ready.take_terminal_probe()?;
        self.used_ready_nonces.push(next.challenge.nonce);
        let probe = std::mem::replace(&mut self.ready_probe, next);
        self.ready_deadline = None;
        self.progress.observe_application_ready(false);
        Ok(TerminalReadinessProbe { probe, observation })
    }
    pub fn observe_session(
        &mut self,
        observed: Option<&crate::desktop::session::SessionBinding>,
    ) -> io::Result<()> {
        let result = self
            .resources
            .session
            .observe(observed)
            .map_err(io::Error::from);
        if result.is_err() {
            self.session_invalid = true;
        }
        self.retain_result(result)
    }
    /// A bounded one-turn poll on the broker's original main/event-loop thread.
    /// Deadlines/cancellation stop waiting, never discard native completion state.
    pub fn poll(&mut self, deadline: Instant, cancel: &AtomicBool) -> io::Result<ActivationEffect> {
        let result = self.poll_inner(deadline, cancel);
        self.retain_result(result)
    }
    /// Advance the original native object once without inventing a wait deadline.
    /// Used by idle/lifetime/retirement observations on the original main thread.
    pub fn poll_native_turn(&mut self) -> io::Result<ActivationEffect> {
        let result = self.poll_native_turn_inner();
        self.retain_result(result)
    }
    fn poll_inner(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<ActivationEffect> {
        if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
            self.progress.mark_deadline_or_cancel();
            return Ok(self.effect());
        }
        self.poll_native_turn_inner()
    }
    fn poll_native_turn_inner(&mut self) -> io::Result<ActivationEffect> {
        if self.local_settled {
            return Ok(self.effect());
        }
        let raw = self
            .resources
            .native
            .ok_or_else(|| invalid("native call was not issued"))?;
        pump_once()?;
        let mut snapshot = Snapshot::default();
        if unsafe { theseus_mac_snapshot(raw.as_ptr(), &mut snapshot) } != 1 {
            return Err(io::Error::other("native completion snapshot unavailable"));
        }
        if snapshot.uncertain_pending != 0 {
            self.progress.mark_native_uncertain();
        }
        if snapshot.status != 0 && !self.terminal_callback_recorded {
            // Retain original callback/object even when files or native metadata
            // no longer match. A race is Unknown, never another launch.
            let intact = self.resources.bundle.revalidate().is_ok();
            let result = match (snapshot.status, intact) {
                (1, true) => CallbackResult::OriginalManagerInstance,
                (2, true) => CallbackResult::Failed,
                _ => CallbackResult::Ambiguous,
            };
            self.progress.callback(result)?;
            self.terminal_callback_recorded = true;
        }
        if snapshot.has_application != 0 && snapshot.terminated != 0 {
            self.progress.manager_terminated();
        }
        self.last_snapshot = snapshot;
        Ok(self.effect())
    }
    pub fn poll_ready(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<ReadinessObservation>> {
        let result = self.probe(false, deadline, cancel);
        self.retain_result(result)
    }
    pub fn poll_request_close(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<ReadinessObservation>> {
        let result = self.probe(true, deadline, cancel);
        self.retain_result(result)
    }
    fn probe(
        &mut self,
        close: bool,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<ReadinessObservation>> {
        self.validate_session()?;
        super::probe::pin_poll_deadline(
            if close {
                &mut self.close_deadline
            } else {
                &mut self.ready_deadline
            },
            deadline,
        )?;
        // Native callback/instance observation has its own one-turn cadence;
        // the original IPC semantic deadline below is never replaced by it.
        self.poll_native_turn()?;
        if self.last_snapshot.status == 0 {
            if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "original native callback remains pending at probe deadline/cancel",
                ));
            }
            return Ok(None);
        }
        if self.last_snapshot.status != 1
            || self.last_snapshot.original_pid <= 0
            || self.last_snapshot.terminated != 0
            || self.effect() == ActivationEffect::Unknown
        {
            return Err(invalid(
                "no exact live original manager instance for cooperative IPC",
            ));
        }
        let CurrentPrincipal::Posix {
            effective_uid,
            effective_gid,
        } = self.resources.session.binding().broker_principal
        else {
            return Err(invalid("non-POSIX native peer"));
        };
        let peer = PeerExpectation {
            uid: effective_uid,
            gid: effective_gid,
            cooperative_instance: self.resources.binding.cooperative_instance.clone(),
            instance_binding_digest: self.resources.binding.instance_binding_digest,
        };
        if close && self.effect() != ActivationEffect::Acknowledged {
            return Err(invalid(
                "cooperative close requires original instance authentication",
            ));
        }
        let Resources {
            binding,
            ready,
            close: close_channel,
            ..
        } = &mut *self.resources;
        let challenge = if close {
            &binding.close
        } else {
            &self.ready_probe.challenge
        };
        let expectation = if close {
            &binding.expectation
        } else {
            &self.ready_probe.expectation
        };
        let channel = if close { close_channel } else { ready };
        let observation = channel.poll_original(
            challenge,
            expectation,
            &peer,
            self.last_snapshot.original_pid as u32,
            deadline,
            cancel,
        )?;
        let Some(observation) = observation else {
            return Ok(None);
        };
        // Check that the same held manager object remains live across exchange.
        self.poll_native_turn()?;
        if self.last_snapshot.terminated != 0 {
            return Err(invalid("original manager instance terminated during IPC"));
        }
        if !close && observation.state != ReadinessState::Incomplete {
            self.progress
                .authenticate_instance(observation.authenticated_instance)?;
            self.progress
                .observe_application_ready(observation.state == ReadinessState::Ready);
        }
        Ok(Some(observation))
    }
    /// Settles only local endpoints/retained callback and original manager
    /// termination. Even success carries zero descendant-scope evidence.
    pub fn try_cleanup(&mut self) -> io::Result<bool> {
        let result = self.try_cleanup_inner();
        self.retain_result(result)
    }
    fn try_cleanup_inner(&mut self) -> io::Result<bool> {
        if self.local_settled {
            return Ok(true);
        }
        if let Some(raw) = self.resources.native {
            self.poll_native_turn()?;
            if self.last_snapshot.status == 0
                || (self.last_snapshot.has_application != 0 && self.last_snapshot.terminated == 0)
            {
                return Ok(false);
            }
            if unsafe { theseus_mac_release_settled(raw.as_ptr()) } != 1 {
                return Ok(false);
            }
            self.resources.native = None;
        }
        self.resources.ready.cleanup()?;
        self.resources.close.cleanup()?;
        self.local_settled = true;
        Ok(true)
    }
}
impl Drop for NativeActivation {
    fn drop(&mut self) {
        // No implicit native calls, broker action replay, UI or close request in
        // Drop. Unresolved application/callback/channel/file owners quarantine.
        if self.local_settled {
            self.resources.mark_settled();
        }
    }
}
