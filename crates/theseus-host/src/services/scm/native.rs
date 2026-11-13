//! Windows-only native boundary. No enumeration, administrator discovery,
//! elevation, account/password APIs, Create/Delete/ChangeConfig or PID operations.
use super::{
    buffer::{self, NativeBuffer},
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

mod ffi;
mod handle;
mod platform;
#[cfg(test)]
mod qualification_tests;
mod query;
mod registry;
mod runtime_pin;

use ffi::*;
use handle::*;
pub use platform::verify_platform_version;
#[cfg(test)]
use platform::{NativeVersionInfo, parse_platform_version};
use query::*;
use registry::*;
use runtime_pin::*;

struct OriginalRegistration {
    handle: RefCell<Option<OwnedHandle>>,
    context: Rc<()>,
    definition: RefCell<ScmDefinition>,
    install_receipt: String,
    configuration_receipts: RefCell<Vec<String>>,
    revoked: Cell<bool>,
    binding_lost: Cell<bool>,
    runtime_pid: Cell<RuntimePin>,
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
            runtime_pid: initial_runtime_pin(observation.status),
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
        // passing exclusively the accepted SUT run's receipt. A transition
        // that leaves the service stopped closes the interval instead: only
        // the helper's own Start may open the next one.
        owner.original.runtime_pid.set(RuntimePin::Open);
        let observation = self.observe_original(&owner.original, deadline, cancel, false)?;
        self.check_owner(owner)?;
        if observation.registration != RegistrationState::Present
            || observation.next_start.as_ref() != Some(definition)
            || observation.status.is_none_or(|s| s.service_type != 0x10)
        {
            self.revoke_context();
            return Err(invalid("unproved SUT SCM configuration transition"));
        }
        close_stopped_interval(&owner.original.runtime_pid, observation.status);
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
            owner.original.runtime_pid.set(RuntimePin::Open);
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
    progress: DeletionProgress,
    next_poll: Instant,
}
impl DeletionOwner {
    pub fn progress(&self) -> DeletionProgress {
        self.progress
    }
}
