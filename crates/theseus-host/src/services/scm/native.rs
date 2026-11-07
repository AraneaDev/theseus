//! Windows-only native boundary. No enumeration, administrator discovery,
//! elevation, account/password APIs, Create/Delete/ChangeConfig or PID operations.
use super::{
    buffer::NativeBuffer,
    model::{ActionTracker, PollBudget, invalid},
    *,
};
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    io,
    mem::{forget, size_of},
    ptr,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

// Narrow ABI subset of winsvc.h. These declarations avoid adding a Services SDK
// feature to the shared manifest; all signatures/layouts match the Windows SDK.
type Handle = *mut c_void;
#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenSCManagerW(machine: *const u16, database: *const u16, access: u32) -> Handle;
    fn OpenServiceW(manager: Handle, name: *const u16, access: u32) -> Handle;
    fn CloseServiceHandle(handle: Handle) -> i32;
    fn QueryServiceConfigW(
        service: Handle,
        buffer: *mut QueryConfig,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    fn QueryServiceConfig2W(
        service: Handle,
        level: u32,
        buffer: *mut u8,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    fn QueryServiceStatusEx(
        service: Handle,
        level: i32,
        buffer: *mut u8,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    fn StartServiceW(service: Handle, argc: u32, argv: *const *const u16) -> i32;
    fn ControlService(service: Handle, control: u32, status: *mut ControlStatus) -> i32;
}
// Read-only registry subset (winreg.h) for the services.exe DeleteFlag.
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        key: Handle,
        subkey: *const u16,
        options: u32,
        sam: u32,
        out: *mut Handle,
    ) -> i32;
    fn RegQueryValueExW(
        key: Handle,
        name: *const u16,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        size: *mut u32,
    ) -> i32;
    fn RegCloseKey(key: Handle) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
}
const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as i32 as isize;
const KEY_QUERY_VALUE: u32 = 0x0001;
const REG_DWORD: u32 = 4;
const ERROR_FILE_NOT_FOUND: i32 = 2;
/// Read-only DeleteFlag of exactly this validated service name. DeleteService
/// makes services.exe set REG_DWORD DeleteFlag=1 on the record it keeps while
/// handles remain open. Opens with KEY_QUERY_VALUE only; never creates/writes.
fn delete_flag_observation(service_name: &str) -> NameObservation {
    // The authority already admits only [A-Za-z0-9._-], max 128, no leading
    // '-'. Refuse anything that could leave the exact Services\<name> key.
    if service_name.is_empty()
        || service_name.len() > 128
        || matches!(service_name, "." | "..")
        || !service_name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        return NameObservation::Unknown(0);
    }
    let path = wide_name(&format!(
        "SYSTEM\\CurrentControlSet\\Services\\{service_name}"
    ));
    let mut key: Handle = ptr::null_mut();
    // SAFETY: nul-terminated owned path; out-pointer is a local.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE as Handle,
            path.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        )
    };
    if opened != 0 {
        return NameObservation::Unknown(opened as u32);
    }
    let value = wide_name("DeleteFlag");
    let (mut kind, mut data, mut size) = (0u32, 0u32, size_of::<u32>() as u32);
    // SAFETY: open key, nul-terminated name, 4-byte local buffer and size.
    let queried = unsafe {
        RegQueryValueExW(
            key,
            value.as_ptr(),
            ptr::null_mut(),
            &mut kind,
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    // SAFETY: closes exactly the key opened above, on every path.
    let closed = unsafe { RegCloseKey(key) };
    if closed != 0 {
        return NameObservation::Unknown(closed as u32);
    }
    match queried {
        0 if kind == REG_DWORD && size == 4 && data == 1 => NameObservation::MarkedForDeletion,
        0 => NameObservation::Unknown(0),
        ERROR_FILE_NOT_FOUND => NameObservation::Present,
        code => NameObservation::Unknown(code as u32),
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
struct QueryConfig {
    service_type: u32,
    start_type: u32,
    error_control: u32,
    binary: *const u16,
    group: *const u16,
    tag: u32,
    dependencies: *const u16,
    account: *const u16,
    display: *const u16,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct NativeFailureAction {
    kind: u32,
    delay: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct FailureActions {
    reset: u32,
    reboot: *const u16,
    command: *const u16,
    count: u32,
    actions: *const NativeFailureAction,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct WidePointer {
    value: *const u16,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct TriggerInfo {
    count: u32,
    triggers: *const c_void,
    reserved: *const c_void,
}
#[repr(C)]
#[derive(Default)]
struct ControlStatus {
    service_type: u32,
    state: u32,
    controls: u32,
    exit: u32,
    specific_exit: u32,
    checkpoint: u32,
    wait_hint: u32,
}
#[repr(C)]
#[derive(Default)]
struct ProcessStatus {
    service_type: u32,
    state: u32,
    controls: u32,
    exit: u32,
    specific_exit: u32,
    checkpoint: u32,
    wait_hint: u32,
    pid: u32,
    flags: u32,
}
// SAFETY: these repr(C) records contain only integers and raw pointers; all
// initialized bit patterns are valid. Every returned interior pointer is bounded
// by NativeBuffer before it is read.
unsafe impl super::buffer::NativeValue for QueryConfig {}
unsafe impl super::buffer::NativeValue for NativeFailureAction {}
unsafe impl super::buffer::NativeValue for FailureActions {}
unsafe impl super::buffer::NativeValue for WidePointer {}
unsafe impl super::buffer::NativeValue for TriggerInfo {}

const INSUFFICIENT_BUFFER: u32 = 122;
const DOES_NOT_EXIST: u32 = 1060;
const MARKED_FOR_DELETE: u32 = 1072;
type Errors = Rc<RefCell<Vec<NativeError>>>;
fn last(api: &'static str) -> NativeError {
    NativeError {
        api,
        code: unsafe { GetLastError() },
    }
}
fn io_native(error: &NativeError) -> io::Error {
    io::Error::from_raw_os_error(error.code as i32)
}
fn keep_error(errors: &Errors, error: NativeError) -> io::Error {
    let result = io_native(&error);
    errors.borrow_mut().push(error);
    result
}
struct OwnedHandle {
    raw: Handle,
    errors: Errors,
}
impl OwnedHandle {
    fn open(raw: Handle, api: &'static str, errors: &Errors) -> io::Result<Self> {
        if raw.is_null() {
            let e = last(api);
            return Err(keep_error(errors, e));
        }
        Ok(Self {
            raw,
            errors: errors.clone(),
        })
    }
    fn close(&mut self) -> io::Result<()> {
        if self.raw.is_null() {
            return Ok(());
        }
        if unsafe { CloseServiceHandle(self.raw) } == 0 {
            let e = last("CloseServiceHandle");
            return Err(keep_error(&self.errors, e));
        }
        self.raw = ptr::null_mut();
        Ok(())
    }
}
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if self.close().is_err() {
            // Failed close is not handle-closure evidence. Preserve diagnostics
            // and the still-open native owner until supplied-context disposal.
            forget(self.errors.clone());
        }
    }
}
struct OriginalRegistration {
    handle: RefCell<Option<OwnedHandle>>,
    context: Rc<()>,
    definition: RefCell<ScmDefinition>,
    install_receipt: String,
    configuration_receipts: RefCell<Vec<String>>,
    revoked: Cell<bool>,
    binding_lost: Cell<bool>,
    runtime_pid: Cell<Option<u32>>,
    last_status: Cell<Option<ServiceStatus>>,
}
impl Drop for OriginalRegistration {
    fn drop(&mut self) {
        // No terminal deletion proof was supplied: closing this handle could
        // change pending-deletion lifetime. Keep the original owner, never reopen.
        if let Some(handle) = self.handle.get_mut().take() {
            forget(handle);
        }
    }
}
/// Not serializable, Clone, Send or reconstructible by name/PID. The retained
/// original SC_HANDLE is the registration identity, including pending deletion.
pub struct RegistrationOwner {
    original: Rc<OriginalRegistration>,
}
impl RegistrationOwner {
    pub fn install_receipt(&self) -> &str {
        &self.original.install_receipt
    }
    pub fn is_revoked(&self) -> bool {
        self.original.revoked.get()
    }
    pub fn original_handle_closed(&self) -> bool {
        self.original.handle.borrow().is_none()
    }
    pub fn next_start_definition(&self) -> ScmDefinition {
        self.original.definition.borrow().clone()
    }
}
struct PendingAction {
    original: Rc<OriginalRegistration>,
    action: ServiceAction,
    budget: PollBudget,
    tracker: ActionTracker,
    progress: OwnedActionProgress,
}
/// Local-only manager, all native calls stay on the caller's existing worker.
/// Before action this instance retains the original object; no same-name service
/// can ever be opened as replacement authority by this manager instance.
pub struct ScmManager {
    manager: Option<OwnedHandle>,
    authority: ScmAuthority,
    context: Rc<()>,
    original: Option<Rc<OriginalRegistration>>,
    partial: Vec<OwnedHandle>,
    qualification_attempted: bool,
    revoked: bool,
    pending: Option<PendingAction>,
    errors: Errors,
}
impl ScmManager {
    pub fn connect(
        authority: ScmAuthority,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        authority.validate()?;
        preflight(deadline, cancel)?;
        let errors = Rc::new(RefCell::new(Vec::new()));
        // NULL machine is intentionally local; no machine/endpoint is accepted.
        // CONNECT (1) only: never ALL_ACCESS/CREATE/ENUMERATE/LOCK.
        let raw = unsafe { OpenSCManagerW(ptr::null(), ptr::null(), 1) };
        let manager = OwnedHandle::open(raw, "OpenSCManagerW", &errors)?;
        preflight(deadline, cancel)?;
        Ok(Self {
            manager: Some(manager),
            authority,
            context: Rc::new(()),
            original: None,
            partial: vec![],
            qualification_attempted: false,
            revoked: false,
            pending: None,
            errors,
        })
    }
    pub fn revoke_context(&mut self) {
        self.revoked = true;
        if let Some(o) = &self.original {
            o.revoked.set(true);
        }
    }
    pub fn is_revoked(&self) -> bool {
        self.revoked
    }
    pub fn retained_errors(&self) -> Vec<NativeError> {
        self.errors.borrow().clone()
    }
    pub fn pending_action(&self) -> Option<OwnedActionProgress> {
        self.pending.as_ref().map(|p| p.progress.clone())
    }
    /// Before qualification this read-only lookup has no mutation rights. After
    /// qualification all observation queries only the retained original handle.
    pub fn inspect(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<ServiceObservation> {
        preflight(deadline, cancel)?;
        if let Some(original) = self.original.clone() {
            return self.observe_original(&original, deadline, cancel, true);
        }
        if let Some(partial) = self.partial.last() {
            // Qualification uncertainty keeps the exact partial native owner
            // available for read-only diagnosis; it never permits name reopening.
            return query_observation(partial.raw, &self.errors, deadline, cancel);
        }
        if self.qualification_attempted {
            return Err(invalid(
                "original SCM qualification unresolved; name reopening refused",
            ));
        }
        let name = wide_name(&self.authority.service_name);
        let raw = unsafe { OpenServiceW(self.manager_raw()?, name.as_ptr(), QUERY_ACCESS) };
        if raw.is_null() {
            let e = last("OpenServiceW(inspect)");
            return match e.code {
                DOES_NOT_EXIST => Ok(ServiceObservation::absent(RegistrationState::Absent)),
                MARKED_FOR_DELETE => {
                    Ok(ServiceObservation::absent(RegistrationState::DeletePending))
                }
                _ => Err(keep_error(&self.errors, e)),
            };
        }
        let handle = OwnedHandle {
            raw,
            errors: self.errors.clone(),
        };
        let result = query_observation(raw, &self.errors, deadline, cancel);
        drop(handle);
        result
    }
    /// Called only after durable publication of original SUT installation. The
    /// supplied receipt is checked by the helper, never inferred from this name.
    pub fn qualify_registration(
        &mut self,
        definition: &ScmDefinition,
        install_receipt: String,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<RegistrationOwner> {
        definition.validate(&self.authority)?;
        receipt(&install_receipt)?;
        preflight(deadline, cancel)?;
        if self.revoked || self.qualification_attempted {
            return Err(invalid(
                "SCM qualification already attempted or context revoked",
            ));
        }
        self.qualification_attempted = true;
        let name = wide_name(&self.authority.service_name);
        let raw = unsafe {
            OpenServiceW(
                self.manager_raw()?,
                name.as_ptr(),
                self.authority.allowed_access,
            )
        };
        let handle = match OwnedHandle::open(raw, "OpenServiceW(qualify)", &self.errors) {
            Ok(h) => h,
            Err(e) => {
                self.revoke_context();
                return Err(e);
            }
        };
        // Publish ownership before any fallible observation. Failure retains this
        // partial native handle and permanently refuses same-name adoption.
        self.partial.push(handle);
        let observation = match query_observation(raw, &self.errors, deadline, cancel) {
            Ok(o) => o,
            Err(e) => {
                self.revoke_context();
                return Err(e);
            }
        };
        if observation.registration != RegistrationState::Present
            || observation.next_start.as_ref() != Some(definition)
            || observation.status.is_none_or(|s| s.service_type != 0x10)
        {
            self.revoke_context();
            return Err(invalid(
                "original SCM service does not match complete admitted configuration",
            ));
        }
        let handle = self.partial.pop().expect("original handle published above");
        let original = Rc::new(OriginalRegistration {
            handle: RefCell::new(Some(handle)),
            context: self.context.clone(),
            definition: RefCell::new(definition.clone()),
            install_receipt,
            configuration_receipts: RefCell::new(vec![]),
            revoked: Cell::new(false),
            binding_lost: Cell::new(false),
            runtime_pid: Cell::new(observation.status.and_then(|s| s.observed_process_id())),
            last_status: Cell::new(observation.status),
        });
        self.original = Some(original.clone());
        Ok(RegistrationOwner { original })
    }
    /// Accept a fully published SUT configuration transition on the SAME original
    /// SC_HANDLE. This does not reopen or replace the registration. A prior
    /// mismatch/revocation can never be healed by a later matching definition.
    pub fn admit_configuration(
        &mut self,
        owner: &mut RegistrationOwner,
        definition: &ScmDefinition,
        sut_receipt: String,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<ServiceObservation> {
        self.check_owner(owner)?;
        definition.validate(&self.authority)?;
        receipt(&sut_receipt)?;
        if self.pending.is_some() {
            return Err(invalid(
                "original action must settle before configuration transition",
            ));
        }
        // The accepted, completed SUT run is the only authority for this
        // transition, and an upgrade may stop and restart the service inside
        // it (stop_for_transition/start_after_transition). Like the helper's
        // own Start, it opens a fresh runtime interval: the previous PID pin
        // is released here, and the first PID observed afterwards is pinned
        // again, so any later unadmitted restart still revokes. The receipt is
        // only format-checked here; its authenticity rests on the helper
        // passing exclusively the accepted SUT run's receipt.
        owner.original.runtime_pid.set(None);
        let observation = self.observe_original(&owner.original, deadline, cancel, false)?;
        self.check_owner(owner)?;
        if observation.registration != RegistrationState::Present
            || observation.next_start.as_ref() != Some(definition)
            || observation.status.is_none_or(|s| s.service_type != 0x10)
        {
            self.revoke_context();
            return Err(invalid("unproved SUT SCM configuration transition"));
        }
        *owner.original.definition.borrow_mut() = definition.clone();
        owner
            .original
            .configuration_receipts
            .borrow_mut()
            .push(sut_receipt);
        Ok(observation)
    }
    pub fn action(
        &mut self,
        owner: &mut RegistrationOwner,
        action: ServiceAction,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<OwnedActionProgress> {
        self.check_owner(owner)?;
        preflight(deadline, cancel)?;
        if self.pending.is_some() {
            return Err(invalid(
                "original SCM action already retained; query it without replay",
            ));
        }
        if !match action {
            ServiceAction::Start => self.authority.allowed_start,
            ServiceAction::Stop => self.authority.allowed_stop,
        } {
            return Err(invalid("SCM runtime action not granted"));
        }
        let budget = PollBudget::new(Instant::now(), deadline)?;
        let observation = self.observe_original(&owner.original, deadline, cancel, true)?;
        // Observation can permanently revoke the binding. Recheck the latch
        // before publishing or invoking any native mutation.
        self.check_owner(owner)?;
        if observation.registration != RegistrationState::Present {
            return Err(invalid("SCM original registration is deleting"));
        }
        let status = observation
            .status
            .ok_or_else(|| invalid("SCM original runtime status unavailable"))?;
        if !matches!(
            (action, status.state()),
            (ServiceAction::Start, RuntimeState::Stopped)
                | (ServiceAction::Stop, RuntimeState::Running)
        ) {
            return Err(invalid("SCM action requires exact stable precondition"));
        }
        preflight(deadline, cancel)?;
        let raw = original_raw(&owner.original)?;
        if action == ServiceAction::Start {
            owner.original.runtime_pid.set(None);
        }
        self.pending = Some(PendingAction {
            original: owner.original.clone(),
            action,
            budget,
            tracker: ActionTracker::new(action, status),
            progress: OwnedActionProgress::Pending {
                action,
                status: Some(status),
            },
        });
        // Pending original owner is published before the synchronous native API.
        // If RPC stalls, cached helper Query/Cancel must remain outside this worker.
        let ok = match action {
            ServiceAction::Start => unsafe { StartServiceW(raw, 0, ptr::null()) },
            ServiceAction::Stop => {
                let mut output = ControlStatus::default();
                unsafe { ControlService(raw, 1, &mut output) }
            }
        };
        if ok == 0 {
            let error = last(match action {
                ServiceAction::Start => "StartServiceW",
                ServiceAction::Stop => "ControlService(STOP)",
            });
            self.errors.borrow_mut().push(error.clone());
            self.revoke_context();
            let progress = OwnedActionProgress::Unknown {
                action,
                status: Some(status),
                reason: "native action error; original outcome requires query, never replay".into(),
                native_error: Some(error),
            };
            self.pending.as_mut().unwrap().progress = progress.clone();
            return Ok(progress);
        }
        self.poll_action(deadline, cancel)
    }
    pub fn poll_action(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<OwnedActionProgress> {
        let mut pending = self
            .pending
            .take()
            .ok_or_else(|| invalid("no original SCM action"))?;
        // Restore the original action slot even if a Rust panic is caught by the
        // helper worker above us; a panic never erases an accepted native effect.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.poll_retained(&mut pending, deadline, cancel)
        }));
        self.pending = Some(pending);
        match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
    fn poll_retained(
        &mut self,
        p: &mut PendingAction,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<OwnedActionProgress> {
        if matches!(p.progress, OwnedActionProgress::Terminal { .. }) {
            return Ok(p.progress.clone());
        }
        let now = Instant::now();
        if now >= deadline || cancel.load(Ordering::Relaxed) {
            self.revoke_context();
            p.progress = OwnedActionProgress::Unknown {
                action: p.action,
                status: p.original.last_status.get(),
                reason: "original SCM deadline/cancellation reached; handle retained".into(),
                native_error: self.errors.borrow().last().cloned(),
            };
            // Subsequent read-only Query can still settle this original object;
            // its original deadline remains recorded and control stays revoked.
            return Ok(p.progress.clone());
        }
        let original_deadline_reached = p.budget.expired(now);
        if original_deadline_reached {
            self.revoke_context();
        }
        if !p.budget.may_query(now) {
            if original_deadline_reached {
                p.progress = OwnedActionProgress::Unknown {
                    action: p.action,
                    status: p.original.last_status.get(),
                    reason: "original SCM deadline reached; original handle retained".into(),
                    native_error: None,
                };
            }
            return Ok(p.progress.clone());
        }
        let raw = original_raw(&p.original)?;
        let mut delete_pending = false;
        match query_definition(raw, &self.errors, deadline, cancel) {
            Ok(config) if config == *p.original.definition.borrow() => {}
            Ok(_) => {
                p.original.binding_lost.set(true);
                self.revoke_context();
            }
            Err(e) if e.raw_os_error() == Some(MARKED_FOR_DELETE as i32) => {
                // SUT deletion does not erase the pending action or original
                // object. Status on this same handle can still prove STOPPED.
                delete_pending = true;
                self.revoke_context();
            }
            Err(e) => {
                self.revoke_context();
                p.progress = OwnedActionProgress::Unknown {
                    action: p.action,
                    status: p.original.last_status.get(),
                    reason: e.to_string(),
                    native_error: self.errors.borrow().last().cloned(),
                };
                return Ok(p.progress.clone());
            }
        }
        let status = match query_status(raw, &self.errors) {
            Ok(s) => s,
            Err(e) => {
                self.revoke_context();
                p.progress = OwnedActionProgress::Unknown {
                    action: p.action,
                    status: p.original.last_status.get(),
                    reason: e.to_string(),
                    native_error: self.errors.borrow().last().cloned(),
                };
                return Ok(p.progress.clone());
            }
        };
        p.original.last_status.set(Some(status));
        if runtime_changed(&p.original, status) {
            p.original.binding_lost.set(true);
            self.revoke_context();
        }
        let original_deadline_reached = original_deadline_reached
            || p.budget.expired(Instant::now())
            || Instant::now() >= deadline
            || cancel.load(Ordering::Relaxed);
        if original_deadline_reached {
            self.revoke_context();
        }
        if p.original.binding_lost.get() {
            p.progress=OwnedActionProgress::Unknown{action:p.action,status:Some(status),reason:"original SCM configuration/runtime binding changed; authority permanently revoked".into(),native_error:None};
            return Ok(p.progress.clone());
        }
        if status.service_type != 0x10 {
            self.revoke_context();
            p.progress = OwnedActionProgress::Unknown {
                action: p.action,
                status: Some(status),
                reason: "original service no longer own-process".into(),
                native_error: None,
            };
            return Ok(p.progress.clone());
        }
        let terminal = if delete_pending
            && p.action == ServiceAction::Start
            && status.state() == RuntimeState::Stopped
        {
            Some(false)
        } else {
            p.tracker.observe(status)
        };
        if let Some(succeeded) = terminal {
            p.progress = OwnedActionProgress::Terminal {
                action: p.action,
                status,
                succeeded,
            };
        } else {
            p.budget
                .observed(now, status.checkpoint, status.wait_hint_ms);
            if original_deadline_reached || p.budget.stalled(now) {
                self.revoke_context();
                p.progress = OwnedActionProgress::Unknown {
                    action: p.action,
                    status: Some(status),
                    reason: if original_deadline_reached {
                        "original SCM deadline reached; query original only"
                    } else {
                        "original SCM checkpoint stalled"
                    }
                    .into(),
                    native_error: None,
                };
            } else {
                p.progress = OwnedActionProgress::Pending {
                    action: p.action,
                    status: Some(status),
                };
            }
        }
        Ok(p.progress.clone())
    }
    /// Release only a native terminal receipt; Unknown cannot authorize replay.
    pub fn take_terminal_action(&mut self) -> io::Result<OwnedActionProgress> {
        if !self
            .pending
            .as_ref()
            .is_some_and(|p| matches!(p.progress, OwnedActionProgress::Terminal { .. }))
        {
            return Err(invalid("SCM original action not terminal"));
        }
        Ok(self.pending.take().unwrap().progress)
    }
    /// This is local handle cleanup after an authenticated, durably accepted SUT
    /// DeleteService, never a helper deletion/stop repair. An original handle is
    /// retained until its original service is observed STOPPED and actions settle.
    pub fn close_after_sut_delete(
        &mut self,
        owner: &mut RegistrationOwner,
        sut_delete_receipt: String,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<DeletionOwner> {
        receipt(&sut_delete_receipt)?;
        preflight(deadline, cancel)?;
        self.belongs(owner)?;
        if self.pending.is_some() {
            return Err(invalid(
                "retain original action before deletion handle closure",
            ));
        }
        let raw = original_raw(&owner.original)?;
        let status = query_status(raw, &self.errors)?;
        owner.original.last_status.set(Some(status));
        if status.service_type != 0x10 || status.state() != RuntimeState::Stopped {
            return Err(invalid(
                "original SCM service not proven stopped before handle closure",
            ));
        }
        preflight(deadline, cancel)?;
        // Revoke BEFORE close. A close failure retains exactly this native owner.
        self.revoke_context();
        let mut handle = owner.original.handle.borrow_mut();
        handle
            .as_mut()
            .ok_or_else(|| invalid("original service handle already closed"))?
            .close()?;
        handle.take();
        Ok(DeletionOwner {
            context: self.context.clone(),
            service_name: wide_name(&self.authority.service_name),
            sut_delete_receipt,
            stopped_original: status,
            progress: DeletionProgress::Pending,
            next_poll: Instant::now(),
        })
    }
    /// Only tests exact absence after the original was safely closed. A present
    /// same-name object is immediately closed read-only and never becomes owner.
    pub fn poll_deletion(
        &mut self,
        owner: &mut DeletionOwner,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<DeletionProgress> {
        preflight(deadline, cancel)?;
        if !Rc::ptr_eq(&self.context, &owner.context) {
            return Err(invalid(
                "deletion observation belongs to another SCM context",
            ));
        }
        if matches!(
            owner.progress,
            DeletionProgress::Complete | DeletionProgress::SameNamePresent
        ) || Instant::now() < owner.next_poll
        {
            return Ok(owner.progress);
        }
        owner.next_poll = Instant::now() + std::time::Duration::from_millis(25);
        let raw = unsafe {
            OpenServiceW(
                self.manager_raw()?,
                owner.service_name.as_ptr(),
                QUERY_ACCESS,
            )
        };
        let name = if raw.is_null() {
            let error = last("OpenServiceW(deletion absence)");
            match error.code {
                DOES_NOT_EXIST => NameObservation::Absent,
                MARKED_FOR_DELETE => NameObservation::MarkedForDeletion,
                code => {
                    self.errors.borrow_mut().push(error);
                    NameObservation::Unknown(code)
                }
            }
        } else {
            let mut observation = OwnedHandle {
                raw,
                errors: self.errors.clone(),
            };
            // OpenServiceW and read-only config queries still succeed on a
            // record that DeleteService marked while handles keep it. Only the
            // exact services.exe DeleteFlag=1 proves that delete-pending
            // record (no new same-name service exists while it does). An absent
            // value means a live same-name object, which stays terminal; any
            // other value, including zero, is Unknown.
            // Nothing is adopted or mutated.
            let name = delete_flag_observation(&self.authority.service_name);
            if let Err(e) = observation.close() {
                owner.progress = DeletionProgress::Unknown(e.raw_os_error().unwrap_or(0) as u32);
                return Ok(owner.progress);
            }
            name
        };
        owner.progress = normalize_deletion(true, true, name);
        Ok(owner.progress)
    }
    fn manager_raw(&self) -> io::Result<Handle> {
        self.manager
            .as_ref()
            .map(|h| h.raw)
            .ok_or_else(|| invalid("SCM manager closed"))
    }
    fn belongs(&self, owner: &RegistrationOwner) -> io::Result<()> {
        if !Rc::ptr_eq(&self.context, &owner.original.context)
            || !self
                .original
                .as_ref()
                .is_some_and(|o| Rc::ptr_eq(o, &owner.original))
        {
            return Err(invalid("foreign original SCM owner"));
        }
        Ok(())
    }
    fn check_owner(&self, owner: &RegistrationOwner) -> io::Result<()> {
        self.belongs(owner)?;
        if self.revoked || owner.is_revoked() || owner.original_handle_closed() {
            return Err(invalid("original SCM authority revoked or handle closed"));
        }
        Ok(())
    }
    fn observe_original(
        &mut self,
        o: &Rc<OriginalRegistration>,
        deadline: Instant,
        cancel: &AtomicBool,
        compare: bool,
    ) -> io::Result<ServiceObservation> {
        let raw = original_raw(o)?;
        let observed = match query_observation(raw, &self.errors, deadline, cancel) {
            Ok(x) => x,
            Err(e) => {
                self.revoke_context();
                return Err(e);
            }
        };
        let changed = observed.status.is_some_and(|s| runtime_changed(o, s));
        if let Some(status) = observed.status {
            o.last_status.set(Some(status));
        }
        if (compare
            && observed.registration == RegistrationState::Present
            && observed.next_start.as_ref() != Some(&*o.definition.borrow()))
            || observed.status.is_some_and(|s| s.service_type != 0x10)
            || changed
        {
            o.binding_lost.set(true);
            self.revoke_context();
        }
        if observed.registration != RegistrationState::Present {
            self.revoke_context();
        }
        Ok(observed)
    }
}
impl Drop for ScmManager {
    fn drop(&mut self) {
        if !self.partial.is_empty()
            || self
                .pending
                .as_ref()
                .is_some_and(|p| !matches!(p.progress, OwnedActionProgress::Terminal { .. }))
        {
            for h in self.partial.drain(..) {
                forget(h);
            }
            if let Some(p) = self.pending.take() {
                forget(p);
            }
            if let Some(h) = self.manager.take() {
                forget(h);
            }
        }
        // OriginalRegistration itself retains any unclosed original handle even
        // on ordinary Drop; only close_after_sut_delete provides closure evidence.
    }
}
/// An absence-observation capability only; it has no service mutation handle.
pub struct DeletionOwner {
    context: Rc<()>,
    service_name: Vec<u16>,
    sut_delete_receipt: String,
    stopped_original: ServiceStatus,
    progress: DeletionProgress,
    next_poll: Instant,
}
impl DeletionOwner {
    pub fn sut_delete_receipt(&self) -> &str {
        &self.sut_delete_receipt
    }
    pub fn stopped_original(&self) -> ServiceStatus {
        self.stopped_original
    }
    pub fn progress(&self) -> DeletionProgress {
        self.progress
    }
}
fn runtime_changed(o: &OriginalRegistration, status: ServiceStatus) -> bool {
    pinned_pid_changed(&o.runtime_pid, status)
}
/// First observed live PID is pinned; any different live PID is a change.
fn pinned_pid_changed(pin: &Cell<Option<u32>>, status: ServiceStatus) -> bool {
    if let Some(pid) = status.observed_process_id() {
        if let Some(previous) = pin.get() {
            return previous != pid;
        }
        pin.set(Some(pid));
    }
    false
}
fn original_raw(o: &OriginalRegistration) -> io::Result<Handle> {
    o.handle
        .borrow()
        .as_ref()
        .map(|h| h.raw)
        .ok_or_else(|| invalid("original SCM handle already closed; reopen forbidden"))
}
fn wide_name(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn receipt(s: &str) -> io::Result<()> {
    if s.is_empty() || s.len() > 4096 || s.chars().any(char::is_control) {
        Err(invalid("missing original SUT receipt"))
    } else {
        Ok(())
    }
}
fn preflight(deadline: Instant, cancel: &AtomicBool) -> io::Result<()> {
    let now = Instant::now();
    if cancel.load(Ordering::Relaxed) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "SCM operation cancelled",
        ));
    }
    if now >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "SCM operation deadline",
        ));
    }
    if deadline.duration_since(now) > std::time::Duration::from_secs(30) {
        return Err(invalid("SCM deadline exceeds 30 seconds"));
    }
    Ok(())
}
fn query_status(raw: Handle, errors: &Errors) -> io::Result<ServiceStatus> {
    let mut output = ProcessStatus::default();
    let mut needed = 0;
    let ok = unsafe {
        QueryServiceStatusEx(
            raw,
            0,
            (&mut output as *mut ProcessStatus).cast(),
            size_of::<ProcessStatus>() as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        let error = last("QueryServiceStatusEx");
        return Err(keep_error(errors, error));
    }
    Ok(ServiceStatus {
        service_type: output.service_type,
        state: output.state,
        controls_accepted: output.controls,
        win32_exit_code: output.exit,
        service_specific_exit_code: output.specific_exit,
        checkpoint: output.checkpoint,
        wait_hint_ms: output.wait_hint,
        process_id: output.pid,
        service_flags: output.flags,
    })
}
fn config_buffer(raw: Handle, level: Option<u32>, errors: &Errors) -> io::Result<NativeBuffer> {
    // Windows documents a maximum 8KiB service configuration output. Allocate a
    // fixed bounded aligned block: no probe/reallocate race or hostile length.
    let mut output = NativeBuffer::new(8192)?;
    let mut needed = 0;
    let ok = match level {
        None => unsafe { QueryServiceConfigW(raw, output.ptr().cast(), output.len(), &mut needed) },
        Some(level) => unsafe {
            QueryServiceConfig2W(raw, level, output.ptr(), output.len(), &mut needed)
        },
    };
    if ok == 0 {
        let error = last(if level.is_none() {
            "QueryServiceConfigW"
        } else {
            "QueryServiceConfig2W"
        });
        if error.code == INSUFFICIENT_BUFFER && needed as usize > 8192 {
            errors.borrow_mut().push(error);
            return Err(invalid("SCM output exceeds native 8KiB bound"));
        }
        return Err(keep_error(errors, error));
    }
    Ok(output)
}
fn query_definition(
    raw: Handle,
    errors: &Errors,
    deadline: Instant,
    cancel: &AtomicBool,
) -> io::Result<ScmDefinition> {
    preflight(deadline, cancel)?;
    let b = config_buffer(raw, None, errors)?;
    let q = b.value::<QueryConfig>()?;
    let mut d = ScmDefinition {
        command_line: b.wide(q.binary)?,
        display_name: b.wide(q.display)?,
        account: b.wide(q.account)?,
        service_type: q.service_type,
        start_type: q.start_type,
        error_control: q.error_control,
        load_order_group: b.wide(q.group)?,
        dependencies: b.multi(q.dependencies)?,
        tag_id: q.tag,
        delayed_auto_start: false,
        failure_actions: vec![],
        failure_reset_period: 0,
        failure_reboot_message: vec![],
        failure_command: vec![],
        failure_actions_on_non_crash: false,
        service_sid_type: 0,
        required_privileges: vec![],
        triggers_present: false,
        launch_protected: 0,
    };
    for level in [2, 3, 4, 5, 6, 8, 12] {
        preflight(deadline, cancel)?;
        let b = config_buffer(raw, Some(level), errors)?;
        match level {
            2 => {
                let f = b.value::<FailureActions>()?;
                d.failure_reset_period = f.reset;
                d.failure_reboot_message = b.wide(f.reboot)?;
                d.failure_command = b.wide(f.command)?;
                d.failure_actions = b
                    .array(f.actions, f.count as usize)?
                    .into_iter()
                    .map(|a| FailureAction {
                        kind: a.kind,
                        delay_ms: a.delay,
                    })
                    .collect();
            }
            3 => d.delayed_auto_start = b.value::<i32>()? != 0,
            4 => d.failure_actions_on_non_crash = b.value::<i32>()? != 0,
            5 => d.service_sid_type = b.value::<u32>()?,
            6 => {
                let p = b.value::<WidePointer>()?;
                d.required_privileges = b.multi(p.value)?;
            }
            8 => d.triggers_present = b.value::<TriggerInfo>()?.count != 0,
            12 => d.launch_protected = b.value::<u32>()?,
            _ => unreachable!(),
        }
    }
    Ok(d)
}
fn query_observation(
    raw: Handle,
    errors: &Errors,
    deadline: Instant,
    cancel: &AtomicBool,
) -> io::Result<ServiceObservation> {
    let config = match query_definition(raw, errors, deadline, cancel) {
        Ok(c) => c,
        Err(e) if e.raw_os_error() == Some(MARKED_FOR_DELETE as i32) => {
            let mut observation = ServiceObservation::absent(RegistrationState::DeletePending);
            observation.status = Some(query_status(raw, errors)?);
            return Ok(observation);
        }
        Err(e) => return Err(e),
    };
    preflight(deadline, cancel)?;
    let status = query_status(raw, errors)?;
    // Separate native reads are not an atomic snapshot. Refuse a changing config
    // across the status observation, preserving the original handle on failure.
    if config != query_definition(raw, errors, deadline, cancel)? {
        return Err(invalid(
            "SCM configuration changed during bounded observation",
        ));
    }
    preflight(deadline, cancel)?;
    Ok(ServiceObservation::present(config, status))
}

#[repr(C)]
struct NativeVersionInfo {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    service_pack: [u16; 128],
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlGetVersion(version: *mut NativeVersionInfo) -> i32;
}
fn parse_platform_version(expected: &str) -> io::Result<[u32; 3]> {
    if expected.len() > 35 {
        return Err(invalid(
            "SCM platform qualification requires canonical major.minor.build",
        ));
    }
    let parts: Vec<&str> = expected.split('.').collect();
    if parts.len() != 3 {
        return Err(invalid(
            "SCM platform qualification requires canonical major.minor.build",
        ));
    }
    let mut values = [0; 3];
    for (part, value) in parts.into_iter().zip(&mut values) {
        if part.is_empty()
            || (part.len() > 1 && part.starts_with('0'))
            || !part.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(invalid(
                "SCM platform qualification has a noncanonical version component",
            ));
        }
        *value = part
            .parse()
            .map_err(|_| invalid("SCM platform qualification component overflow"))?;
    }
    Ok(values)
}
/// Verify the actual local Windows version/build before any SCM connection.
/// A receipt string or process compatibility manifest is not a native version
/// measurement. RtlGetVersion returns an NTSTATUS, never a Win32 error domain.
pub fn verify_platform_version(expected: &str) -> io::Result<()> {
    let expected = parse_platform_version(expected)?;
    let mut version = NativeVersionInfo {
        size: size_of::<NativeVersionInfo>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        service_pack: [0; 128],
    };
    // SAFETY: initialized SDK-compatible RTL_OSVERSIONINFOW storage of exactly
    // the advertised size remains alive and exclusively borrowed for the call.
    let status = unsafe { RtlGetVersion(&mut version) };
    if status < 0 {
        return Err(io::Error::other(format!(
            "RtlGetVersion NTSTATUS 0x{:08x}",
            status as u32
        )));
    }
    if version.size != size_of::<NativeVersionInfo>() as u32
        || version.platform != 2
        || expected != [version.major, version.minor, version.build]
    {
        return Err(invalid(
            "actual Windows version/build differs from supplied SCM qualification",
        ));
    }
    Ok(())
}
#[cfg(test)]
mod qualification_tests {
    use super::*;
    #[test]
    fn delete_flag_reads_only_the_exact_validated_service_key() {
        for name in ["", ".", "..", "a\\b", "a/b", "..\\Tcpip", "x\0"] {
            assert_eq!(delete_flag_observation(name), NameObservation::Unknown(0));
        }
        // A real absent key is an error, never Present or MarkedForDeletion.
        assert_eq!(
            delete_flag_observation("TheseusAbsentDeleteFlagProbe_7f3a"),
            NameObservation::Unknown(2)
        );
    }
    #[test]
    fn released_runtime_pin_admits_exactly_one_new_pid_then_detects_changes() {
        let status = |state, process_id| ServiceStatus {
            service_type: 0x10,
            state,
            controls_accepted: 1,
            win32_exit_code: 0,
            service_specific_exit_code: 0,
            checkpoint: 0,
            wait_hint_ms: 0,
            process_id,
            service_flags: 0,
        };
        let pin = Cell::new(None);
        assert!(!pinned_pid_changed(&pin, status(4, 8656)));
        // A restart that is not released (no admitted transition) is a change.
        assert!(pinned_pid_changed(&pin, status(4, 9001)));
        // Stopped carries no live PID and never pins or changes.
        assert!(!pinned_pid_changed(&pin, status(1, 0)));
        // Admitted SUT transition releases the pin: the new PID is pinned once,
        assert!(pin.replace(None).is_some());
        assert!(!pinned_pid_changed(&pin, status(4, 9001)));
        assert_eq!(pin.get(), Some(9001));
        // and any later unadmitted restart is still detected.
        assert!(pinned_pid_changed(&pin, status(4, 9100)));
    }
    #[test]
    fn platform_version_parser_is_canonical_and_never_selects_a_default() {
        assert_eq!(
            parse_platform_version("10.0.26100").unwrap(),
            [10, 0, 26100]
        );
        for invalid in [
            "",
            "10",
            "10.0",
            "10.0.26100.extra",
            "10.00.26100",
            "10.0.-1",
            "10.0.4294967296",
            " 10.0.26100",
        ] {
            assert!(parse_platform_version(invalid).is_err());
        }
        assert_eq!(size_of::<NativeVersionInfo>(), 276);
    }
}
