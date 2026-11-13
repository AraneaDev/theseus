//! Native Shell open on the admitted STA broker. The original returned HANDLE
//! is the sole primary owner. No OpenProcess, job assignment or terminate call.
use super::{ActivationEffect, ActivationProgress, AdmittedShortcut, com::win32, invalid};
use crate::{
    desktop::{
        owned::{CommittedReadinessProbe, ExecutableBinding, TerminalReadinessProbe},
        session::{BoundSession, DesktopKind, SessionBinding, SuppliedDisplay},
    },
    principal::CurrentPrincipal,
    services::readiness::{LocalChallenge, PipePeerExpectation, ReadinessObservation},
};
use std::{
    ffi::c_void,
    io,
    mem::{size_of, zeroed},
    os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_NO_TOKEN, FILETIME, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::TOKEN_QUERY,
    System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, GetCurrentThread, GetCurrentThreadId,
        GetProcessTimes, OpenThreadToken, QueryFullProcessImageNameW, WaitForSingleObject,
    },
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeLifetime {
    ActivationInstanceTerminated,
    OwnedPrimaryExited,
    OwnedProcessTreeSettled,
}
#[repr(C)]
struct ShellInfo {
    size: u32,
    mask: u32,
    window: HANDLE,
    verb: *const u16,
    file: *const u16,
    parameters: *const u16,
    directory: *const u16,
    show: i32,
    instance: HANDLE,
    id_list: *mut c_void,
    class: *const u16,
    class_key: HANDLE,
    hotkey: u32,
    icon: HANDLE,
    process: HANDLE,
}
#[repr(C)]
struct UserFlags {
    inherit: i32,
    reserved: i32,
    flags: u32,
}
#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteExW(info: *mut ShellInfo) -> i32;
}
#[link(name = "user32")]
unsafe extern "system" {
    fn GetProcessWindowStation() -> HANDLE;
    fn GetThreadDesktop(thread: u32) -> HANDLE;
    fn GetUserObjectInformationW(
        object: HANDLE,
        index: i32,
        buffer: *mut c_void,
        length: u32,
        needed: *mut u32,
    ) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
    fn GetSystemTimeAsFileTime(time: *mut FILETIME);
}
fn wide(v: &str) -> Vec<u16> {
    v.encode_utf16().chain(Some(0)).collect()
}
fn user_name(handle: HANDLE) -> io::Result<String> {
    if handle.is_null() {
        return Err(win32("current window station/desktop"));
    }
    let mut buffer = [0u16; 256];
    let mut needed = 0;
    if unsafe {
        GetUserObjectInformationW(
            handle,
            2,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 2) as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(win32("GetUserObjectInformationW(UOI_NAME)"));
    }
    let n = buffer
        .iter()
        .position(|v| *v == 0)
        .ok_or_else(|| invalid("unterminated native desktop name"))?;
    String::from_utf16(&buffer[..n]).map_err(|_| invalid("non Unicode desktop name"))
}
fn process_session(pid: u32) -> io::Result<u32> {
    let mut session = 0;
    if unsafe { ProcessIdToSessionId(pid, &mut session) } == 0 {
        return Err(win32("ProcessIdToSessionId(original metadata)"));
    }
    Ok(session)
}
fn no_impersonation() -> io::Result<()> {
    let mut token = ptr::null_mut();
    let success = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
    if success != 0 {
        let _token = unsafe { OwnedHandle::from_raw_handle(token) };
        return Err(invalid("impersonated broker thread is unsupported"));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
        return Err(win32("OpenThreadToken(no impersonation)"));
    }
    Ok(())
}
/// Measures only the calling broker's current placement; never enumerates,
/// switches, opens or searches another session/window station/desktop.
fn validate_session(session: &BoundSession) -> io::Result<u32> {
    session.authorize_native_adapter(DesktopKind::Windows)?;
    no_impersonation()?;
    let SuppliedDisplay::WindowsInteractive {
        session_id,
        window_station,
        desktop,
    } = &session.binding().display
    else {
        return Err(invalid(
            "native shell requires supplied interactive Windows session",
        ));
    };
    if *session_id == 0 || process_session(unsafe { GetCurrentProcessId() })? != *session_id {
        return Err(invalid("broker is outside supplied nonzero session"));
    }
    let station = unsafe { GetProcessWindowStation() };
    let current_desktop = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
    if user_name(station)? != *window_station || user_name(current_desktop)? != *desktop {
        return Err(invalid("broker window station/desktop differs"));
    }
    let mut flags = UserFlags {
        inherit: 0,
        reserved: 0,
        flags: 0,
    };
    let mut needed = 0;
    if unsafe {
        GetUserObjectInformationW(
            station,
            1,
            (&mut flags as *mut UserFlags).cast(),
            size_of::<UserFlags>() as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(win32("GetUserObjectInformationW(UOI_FLAGS)"));
    }
    if flags.flags & 1 == 0 {
        return Err(invalid("broker window station is not interactive"));
    }
    let (_pid, sid) = LocalChallenge::original_process_metadata(unsafe {
        BorrowedHandle::borrow_raw(GetCurrentProcess())
    })?;
    let actual = CurrentPrincipal::Windows {
        token_user_sid: sid.as_str().to_owned(),
    };
    if actual != session.binding().broker_principal {
        return Err(invalid(
            "broker actual process account differs from supplied binding",
        ));
    }
    Ok(*session_id)
}
#[derive(Clone, Debug)]
pub struct LifetimeObservation {
    pub effect: ActivationEffect,
    pub returned_original_process: bool,
    pub fresh_instance_authenticated: bool,
    pub primary_exited: bool,
    pub descendants_owned: bool,
    pub cleanup_complete: bool,
    pub session_revoked: bool,
}
struct Resources {
    session: BoundSession,
    entry: AdmittedShortcut,
    binding: ExecutableBinding,
    ready_probe: CommittedReadinessProbe,
    used_ready_nonces: Vec<[u8; 16]>,
    ready_deadline: Option<Instant>,
    close_deadline: Option<Instant>,
    ready: LocalChallenge,
    close: LocalChallenge,
    process: Option<OwnedHandle>,
    peer: Option<PipePeerExpectation>,
    session_id: u32,
}
pub struct NativeActivation {
    resources: Option<Resources>,
    progress: ActivationProgress,
    errors: Vec<io::Error>,
    errors_truncated: bool,
}
pub struct NativeStartFailure {
    pub error: io::Error,
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
impl NativeActivation {
    /// Caller has durably accepted the original action before this native effect.
    /// This synchronous SDK boundary stays on the original COM thread. A broker
    /// must retain that thread when its transport/action deadline expires.
    #[allow(clippy::too_many_arguments)]
    pub fn begin(
        session: BoundSession,
        entry: AdmittedShortcut,
        binding: ExecutableBinding,
        ready: LocalChallenge,
        close: LocalChallenge,
        lifetime: NativeLifetime,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Self, NativeStartFailure> {
        let mut owner = Self {
            resources: Some(Resources {
                session,
                entry,
                ready_probe: CommittedReadinessProbe {
                    challenge: binding.ready.clone(),
                    expectation: binding.expectation.clone(),
                },
                used_ready_nonces: vec![binding.ready.nonce],
                ready_deadline: None,
                close_deadline: None,
                binding,
                ready,
                close,
                process: None,
                peer: None,
                session_id: 0,
            }),
            progress: ActivationProgress::pending(),
            errors: Vec::new(),
            errors_truncated: false,
        };
        let result = (|| {
            let r = owner.resources.as_mut().unwrap();
            if lifetime == NativeLifetime::OwnedProcessTreeSettled {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "native shell cannot own a private descendant tree",
                ));
            }
            if deadline.saturating_duration_since(Instant::now())
                > std::time::Duration::from_secs(30)
            {
                return Err(invalid("native activation deadline exceeds 30 seconds"));
            }
            if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native activation deadline/cancel before effect",
                ));
            }
            r.binding.validate()?;
            r.session_id = validate_session(&r.session)?;
            r.entry.revalidate()?;
            if r.ready.endpoint() == r.close.endpoint() {
                return Err(invalid(
                    "native readiness/close require separate original channels",
                ));
            }
            if r.binding.expectation.artifact_length != r.entry.profile().executable_artifact.length
                || r.binding.expectation.artifact_sha256
                    != r.entry.profile().executable_artifact.sha256
            {
                return Err(invalid(
                    "readiness executable differs from admitted shortcut target",
                ));
            }
            super::probe::validate_static_bootstrap(
                &r.entry.profile().arguments,
                r.ready.endpoint().as_str(),
                r.close.endpoint().as_str(),
            )?;
            let path = wide(
                r.entry
                    .retained
                    .entry_path()
                    .to_str()
                    .ok_or_else(|| invalid("non Unicode shortcut path"))?,
            );
            let cwd = wide(&r.entry.profile().root);
            let verb = wide("open");
            let mut info: ShellInfo = unsafe { zeroed() };
            info.size = size_of::<ShellInfo>() as u32;
            info.mask = 0x40 | 0x100 | 0x400;
            info.verb = verb.as_ptr();
            info.file = path.as_ptr();
            info.directory = cwd.as_ptr();
            info.show = 1;
            let mut before = FILETIME::default();
            unsafe {
                GetSystemTimeAsFileTime(&mut before);
            }
            // No runas, association class, IDList, DDE conversation or URL input.
            // Explicit asInvoker was admitted before this call; NO_UI alone is
            // never treated as a UAC prohibition.
            let status = unsafe { ShellExecuteExW(&mut info) };
            let native_error = if status == 0 {
                Some(win32("ShellExecuteExW(open exact .lnk)"))
            } else {
                None
            };
            if !info.process.is_null() && info.process != INVALID_HANDLE_VALUE {
                r.process = Some(unsafe { OwnedHandle::from_raw_handle(info.process) });
            }
            owner.progress.native_return(r.process.is_some())?;
            if let Some(error) = native_error {
                return Err(error);
            }
            let process = r.process.as_ref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Shell returned no original process object; delegated lifetime unavailable",
                )
            })?;
            let (pid, sid) = LocalChallenge::original_process_metadata(process.as_handle())?;
            if process_session(pid)? != r.session_id
                || (CurrentPrincipal::Windows {
                    token_user_sid: sid.as_str().to_owned(),
                }) != r.session.binding().broker_principal
            {
                return Err(invalid("returned process account/session differs"));
            }
            let mut image = [0u16; 2048];
            let mut length = image.len() as u32;
            if unsafe {
                QueryFullProcessImageNameW(
                    process.as_raw_handle(),
                    0,
                    image.as_mut_ptr(),
                    &mut length,
                )
            } == 0
            {
                return Err(win32("QueryFullProcessImageNameW(original)"));
            }
            let image = String::from_utf16(&image[..length as usize])
                .map_err(|_| invalid("non Unicode original process image"))?;
            if image != r.entry.retained.executable_path().to_str().unwrap() {
                return Err(invalid("Shell process image differs from pinned target"));
            }
            let mut created = FILETIME::default();
            let mut exit = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            if unsafe {
                GetProcessTimes(
                    process.as_raw_handle(),
                    &mut created,
                    &mut exit,
                    &mut kernel,
                    &mut user,
                )
            } == 0
            {
                return Err(win32("GetProcessTimes(original)"));
            }
            let ticks =
                |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
            if ticks(created) < ticks(before) {
                owner.progress.authenticate_new_instance(false)?;
                return Err(invalid("Shell returned a preexisting process object"));
            }
            r.peer = Some(PipePeerExpectation {
                sid,
                original_pid: pid,
                cooperative_instance: r.binding.cooperative_instance.clone(),
                instance_binding_digest: r.binding.instance_binding_digest,
            });
            if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
                owner.progress.mark_deadline_or_cancel();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native call completed after deadline; original owner retained",
                ));
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok(owner),
            Err(error) => Err(NativeStartFailure {
                error,
                owner: Box::new(owner),
            }),
        }
    }
    pub fn observation(&self) -> LifetimeObservation {
        LifetimeObservation {
            effect: self.progress.effect(),
            returned_original_process: self.progress.owned_primary_available(),
            fresh_instance_authenticated: self.progress.new_instance_proved(),
            primary_exited: self.progress.primary_exited(),
            descendants_owned: false,
            cleanup_complete: self.resources.is_none(),
            session_revoked: self
                .resources
                .as_ref()
                .is_some_and(|r| r.session.is_revoked()),
        }
    }
    pub fn observe_session(&mut self, observed: Option<&SessionBinding>) -> io::Result<()> {
        let r = self
            .resources
            .as_mut()
            .ok_or_else(|| invalid("native owner settled"))?;
        r.session.observe(observed)?;
        validate_session(&r.session)?;
        Ok(())
    }
    pub fn poll_ready(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<ReadinessObservation>> {
        let r = self
            .resources
            .as_mut()
            .ok_or_else(|| invalid("native owner settled"))?;
        validate_session(&r.session)?;
        super::probe::pin_poll_deadline(&mut r.ready_deadline, deadline)?;
        let peer = r
            .peer
            .as_ref()
            .ok_or_else(|| invalid("no admitted original Shell process peer"))?;
        let process = r
            .process
            .as_ref()
            .ok_or_else(|| invalid("no original Shell process"))?;
        let observed = r.ready.poll_original_process(
            &r.ready_probe.challenge,
            &r.ready_probe.expectation,
            peer,
            process.as_handle(),
            deadline,
            cancel,
        )?;
        if let Some(observed) = &observed {
            super::probe::authenticate_observed_instance(&mut self.progress, observed)?;
        }
        Ok(observed)
    }
    pub fn current_ready_probe(&self) -> Option<&CommittedReadinessProbe> {
        self.resources.as_ref().map(|r| &r.ready_probe)
    }
    /// Consumes only an actually terminal prior native IPC sample. The return
    /// value carries the OLD nonce/facts; the next sample needs its own exchange.
    pub fn begin_fresh_ready_probe(
        &mut self,
        next: CommittedReadinessProbe,
    ) -> io::Result<TerminalReadinessProbe> {
        let r = self
            .resources
            .as_mut()
            .ok_or_else(|| invalid("native owner settled"))?;
        super::probe::validate_fresh_probe(&r.binding, &next, &r.used_ready_nonces)?;
        let observation = r.ready.take_terminal_probe()?;
        let old = std::mem::replace(&mut r.ready_probe, next);
        r.ready_deadline = None;
        r.used_ready_nonces.push(r.ready_probe.challenge.nonce);
        Ok(TerminalReadinessProbe {
            probe: old,
            observation,
        })
    }
    pub fn poll_request_close(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<ReadinessObservation>> {
        if !self.progress.new_instance_proved() {
            return Err(invalid(
                "cooperative close requires authenticated original new instance",
            ));
        }
        let r = self
            .resources
            .as_mut()
            .ok_or_else(|| invalid("native owner settled"))?;
        validate_session(&r.session)?;
        super::probe::pin_poll_deadline(&mut r.close_deadline, deadline)?;
        r.close.poll_original_process(
            &r.binding.close,
            &r.binding.expectation,
            r.peer
                .as_ref()
                .ok_or_else(|| invalid("missing original peer"))?,
            r.process
                .as_ref()
                .ok_or_else(|| invalid("missing original process"))?
                .as_handle(),
            deadline,
            cancel,
        )
    }
    /// Advance only the retained shortcut's original STA on this same thread.
    pub fn pump_once(&mut self) -> io::Result<()> {
        match &self.resources {
            Some(r) => r.entry.pump_once(),
            None => Ok(()),
        }
    }
    pub fn observe_primary(&mut self) -> io::Result<bool> {
        let Some(r) = self.resources.as_ref() else {
            return Ok(self.progress.primary_exited());
        };
        let Some(process) = r.process.as_ref() else {
            return Ok(false);
        };
        match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => {
                self.progress.observe_primary_exit();
                Ok(true)
            }
            WAIT_TIMEOUT => Ok(false),
            _ => Err(win32("WaitForSingleObject(original primary)")),
        }
    }
    pub fn try_cleanup(&mut self) -> io::Result<bool> {
        if self.resources.is_none() {
            return Ok(true);
        }
        let has_process = self.resources.as_ref().unwrap().process.is_some();
        if has_process && !self.observe_primary()? {
            return Ok(false);
        }
        let r = self.resources.as_mut().unwrap();
        let ready = r.ready.cleanup();
        let close = r.close.cleanup();
        if let Err(error) = ready {
            self.retain_error(error);
            return Ok(false);
        }
        if let Err(error) = close {
            self.retain_error(error);
            return Ok(false);
        }
        self.resources.take();
        Ok(true)
    }
    fn retain_error(&mut self, error: io::Error) {
        if self.errors.len() < 16 {
            self.errors.push(error);
        } else {
            self.errors_truncated = true;
        }
    }
    pub fn retained_errors(&self) -> &[io::Error] {
        &self.errors
    }
    pub fn errors_truncated(&self) -> bool {
        self.errors_truncated
    }
}
impl Drop for NativeActivation {
    fn drop(&mut self) {
        if !matches!(self.try_cleanup(), Ok(true))
            && let Some(resources) = self.resources.take()
        {
            std::mem::forget(resources);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        desktop::session::{BrokerEndpoint, DesktopMode, SessionAuthority},
        services::readiness::NamedPipe,
    };
    fn authority(display: SuppliedDisplay, modes: Vec<DesktopMode>) -> SessionAuthority {
        SessionAuthority {
            kind: DesktopKind::Windows,
            binding: SessionBinding {
                target_id: "fixture".into(),
                ownership_id: [1; 16],
                provider_epoch: [2; 32],
                session_id: "source-only".into(),
                session_epoch: [3; 32],
                broker_header_digest: [4; 32],
                manifest_digest: [5; 32],
                broker_endpoint: BrokerEndpoint::Pipe(
                    NamedPipe::new(r"\\.\pipe\theseus.explicit-session-fixture".into()).unwrap(),
                ),
                broker_principal: CurrentPrincipal::Windows {
                    token_user_sid: "S-1-5-18".into(),
                },
                helper_principal: CurrentPrincipal::Windows {
                    token_user_sid: "S-1-5-18".into(),
                },
                display,
            },
            allowed_modes: modes,
        }
    }
    #[test]
    fn windows_native_session_zero_headless_and_actual_wrong_session_refuse_without_shell() {
        let zero = authority(
            SuppliedDisplay::WindowsInteractive {
                session_id: 0,
                window_station: "WinSta0".into(),
                desktop: "Default".into(),
            },
            vec![DesktopMode::NativeActivation],
        );
        assert!(BoundSession::admit(zero.clone(), &zero.binding).is_err());
        let headless = authority(
            SuppliedDisplay::SourceOwnedHeadless,
            vec![DesktopMode::OwnedExecutable],
        );
        let session = BoundSession::admit(headless.clone(), &headless.binding).unwrap();
        assert!(validate_session(&session).is_err());
        // Only measure this process's own session; no external-session lookup.
        let current = process_session(unsafe { GetCurrentProcessId() }).unwrap();
        let other = current.checked_add(1).unwrap();
        let wrong = authority(
            SuppliedDisplay::WindowsInteractive {
                session_id: other,
                window_station: "WinSta0".into(),
                desktop: "Default".into(),
            },
            vec![DesktopMode::NativeActivation],
        );
        let session = BoundSession::admit(wrong.clone(), &wrong.binding).unwrap();
        assert!(validate_session(&session).is_err());
    }
}
