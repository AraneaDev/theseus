//! Exact Linux desktop-entry admission and native GIO activation.
//!
//! This is a mechanism for the already authenticated, durably accepted broker
//! operation. It never installs/repairs an entry or discovers a session. Native
//! launch acknowledgement alone supplies no owned-process or cleanup authority.
mod artifact;
mod definition;
mod environment;
mod qualification;
mod reaping;
mod sdk;
use crate::desktop::session::{BoundSession, DesktopKind, SessionBinding};
use artifact::RetainedArtifact;
pub use artifact::RetainedEntry;
#[cfg(test)]
use artifact::hash_bytes;
use artifact::hash_file;
pub use definition::{AdmittedDefinition, ArtifactSlot, LiteralCommand};
use definition::{absolute, parse_entry};
use environment::validate_environment;
use qualification::validate_compiled_binary;
pub use qualification::{
    CompiledArtifactQualification, NativeBinaryAdmission, NativeElfAbi, ObservedNativeBinary,
    QualificationProvenance, QualifiedGioSdk, QualifiedSdkImage, QualifiedSdkImages, SdkImageRole,
};
#[cfg(test)]
use reaping::ChildWait;
use reaping::{
    ChildReservation, OriginalGioChild, OriginalInvocation, require_no_existing_children,
    verify_wait_contract,
};
#[cfg(all(test, feature = "desktop-test-support"))]
use reaping::{reap_reserved_child, wait_reserved_child};
pub use sdk::{DynamicLoaderFailure, NativeErrorOwner};
use std::{collections::BTreeMap, io};

const MAX_DEFINITION: u64 = 65_536;
const MAX_ARTIFACT: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeLifetimeRequirement {
    AcknowledgementOnly,
    OwnedPrimary,
    OwnedTree,
}
fn validate_lifetime(requirement: NativeLifetimeRequirement) -> io::Result<()> {
    if requirement == NativeLifetimeRequirement::OwnedTree {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "GIO primary reservation supplies no native tree authority",
        ));
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeLaunchReceipt {
    /// SDK acknowledgement plus diagnostic spawn metadata, never process rights,
    /// readiness, fresh application instance, primary exit or cleanup evidence.
    AcknowledgedSpawnCallback {
        diagnostic_pid: i32,
    },
    FailedOrPartial,
    Ambiguous,
}
#[derive(Default)]
struct NativeCallbacks {
    count: u32,
    pid: Option<i32>,
    pids: [i32; MAX_CALLBACKS],
    stored: usize,
    overflow: bool,
}
impl NativeCallbacks {
    fn record(&mut self, pid: i32) {
        self.count = self.count.saturating_add(1);
        if self.stored < MAX_CALLBACKS {
            self.pids[self.stored] = pid;
            self.stored += 1;
        } else {
            self.overflow = true;
        }
        if self.count == 1 && pid > 0 {
            self.pid = Some(pid);
        } else {
            self.pid = None;
        }
    }
    fn receipt(&self, acknowledged: bool) -> NativeLaunchReceipt {
        if !acknowledged {
            return NativeLaunchReceipt::FailedOrPartial;
        }
        match (self.count, self.pid) {
            (1, Some(diagnostic_pid)) => {
                NativeLaunchReceipt::AcknowledgedSpawnCallback { diagnostic_pid }
            }
            _ => NativeLaunchReceipt::Ambiguous,
        }
    }
}

const MAX_CALLBACKS: usize = 16;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativePrimaryState {
    Unavailable,
    Running,
    Exited,
    Unknown,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Non-Send capability for a dedicated broker with an explicitly replaced
/// startup environment, before GIO/GLib's process-global session caches exist.
pub struct DedicatedGioBroker {
    session: BoundSession,
    environment: Vec<(String, String)>,
    helper: RetainedArtifact,
    helper_admission: NativeBinaryAdmission,
    sdk: QualifiedGioSdk,
    sdk_images: Option<std::rc::Rc<sdk::images::PinnedImages>>,
    revoked: bool,
    process: libc::pid_t,
    _worker: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl DedicatedGioBroker {
    /// # Safety
    /// Only call in an explicitly owned dedicated broker process before ANY
    /// GIO/GLib initialization. The supplied environment must be the process's
    /// complete replacement startup environment. No other user of GIO/session
    /// caches, activation, environment writer or child job may coexist for this
    /// process's lifetime. No other child creator/reaper, child watch, SIGCHLD
    /// handler/SA_NOCLDWAIT change or concurrent wait may exist. The broker owns
    /// the exact SDK bus connection exclusively from initial setup through
    /// flush/close completion. The authenticated caller must guarantee exclusive,
    /// quiescent entry/executable/SDK namespaces through native completion; the
    /// before/after checks do not defeat a hostile concurrent namespace writer.
    /// Both helper and every app qualification later supplied to `prepare` must
    /// be authenticated independently from verified provider/build provenance
    /// or an original native version Run of the SAME immutable artifact. The
    /// receipt must bind exact length/SHA/native ABI and this qualified SDK. The
    /// caller guarantees these are valid compiled host binaries, never arbitrary
    /// executables/scripts or claims derived from the entry/application. ELF
    /// headers and hashes alone cannot suppress GIO/libc ENOEXEC shell fallback;
    /// this contract excludes that unqualified subset before native effects.
    /// Never call from the controller, helper, test runner or an ambient session.
    pub unsafe fn admit(
        session: BoundSession,
        environment: Vec<(String, String)>,
        helper: ArtifactSlot,
        helper_qualification: Option<CompiledArtifactQualification>,
        sdk: QualifiedGioSdk,
    ) -> io::Result<Self> {
        session.authorize_native_adapter(DesktopKind::Linux)?;
        sdk.validate()?;
        if sdk.helper_length != helper.length || sdk.helper_sha256 != helper.sha256 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "SDK helper differs from independently qualified commitment",
            ));
        }
        let helper = RetainedArtifact::admit(helper, MAX_ARTIFACT, true)?;
        let helper_admission = validate_compiled_binary(
            &helper.file,
            &helper.slot,
            helper_qualification.as_ref(),
            &sdk,
        )?;
        let helper_path = helper.path();
        let helper_path = helper_path
            .to_str()
            .ok_or_else(|| invalid("non-UTF8 SDK helper path"))?;
        validate_environment(&session, &environment, helper_path)?;
        let broker = Self {
            session,
            environment,
            helper,
            helper_admission,
            sdk,
            sdk_images: None,
            revoked: false,
            process: unsafe { libc::getpid() },
            _worker: std::marker::PhantomData,
        };
        broker.revalidate()?;
        require_no_existing_children()?;
        Ok(broker)
    }
    /// Exact image variant of `admit`. The identical unsafe dedicated startup,
    /// original provider provenance and exclusive loader/SDK namespace contract
    /// applies to every image. No SDK has been initialized before this call.
    /// # Safety
    /// Discharge every obligation of `admit`; `images` is the complete original
    /// independently qualified SDK closure, not an observed/discovered library list.
    pub unsafe fn admit_with_images(
        session: BoundSession,
        environment: Vec<(String, String)>,
        helper: ArtifactSlot,
        helper_qualification: Option<CompiledArtifactQualification>,
        sdk: QualifiedGioSdk,
        images: QualifiedSdkImages,
    ) -> io::Result<Self> {
        let mut original =
            unsafe { Self::admit(session, environment, helper, helper_qualification, sdk) }?;
        original.sdk_images = Some(std::rc::Rc::new(sdk::images::PinnedImages::admit(images)?));
        original.revalidate()?;
        Ok(original)
    }
    fn revalidate(&self) -> io::Result<()> {
        if self.revoked {
            return Err(invalid("original SDK token authority revoked"));
        }
        self.session.authorize_native_adapter(DesktopKind::Linux)?;
        verify_wait_contract(self.process)?;
        if self.process != unsafe { libc::getpid() }
            || unsafe { libc::getuid() != libc::geteuid() || libc::getgid() != libc::getegid() }
            // GLib ignores GIO_LAUNCH_DESKTOP in secure-exec mode even if a
            // broker has already dropped its effective IDs. Never fall back to
            // an unpinned build-time/PATH helper in that process.
            || unsafe { libc::getauxval(libc::AT_SECURE) } != 0
            || crate::principal::current_principal()? != self.session.binding().broker_principal
        {
            return Err(invalid(
                "GIO broker original process/principal differs from supplied session",
            ));
        }
        let current: BTreeMap<_, _> = std::env::vars_os().collect();
        let expected: BTreeMap<_, _> = self
            .environment
            .iter()
            .map(|(k, v)| (std::ffi::OsString::from(k), std::ffi::OsString::from(v)))
            .collect();
        if current != expected {
            return Err(invalid(
                "broker environment differs from explicit replacement startup environment",
            ));
        }
        validate_environment(
            &self.session,
            &self.environment,
            self.helper
                .path()
                .to_str()
                .ok_or_else(|| invalid("invalid helper path"))?,
        )?;
        if let Some(images) = &self.sdk_images {
            images.revalidate()?;
        }
        self.helper.revalidate()
    }
    pub fn observe_session(&mut self, observed: Option<&SessionBinding>) -> io::Result<()> {
        self.session.observe(observed).map_err(Into::into)
    }
    pub fn matches_qualified_sdk_images(&self, images: &QualifiedSdkImages) -> bool {
        self.sdk_images
            .as_ref()
            .is_some_and(|original| original.matches(images))
    }
    pub fn session_binding(&self) -> &SessionBinding {
        self.session.binding()
    }
    pub fn helper_admission(&self) -> &NativeBinaryAdmission {
        &self.helper_admission
    }
    pub fn sdk_qualification(&self) -> &QualifiedGioSdk {
        &self.sdk
    }
    /// Caller has durably published the exact original operation, session and
    /// independently admitted definition under this manifest before preparation.
    pub fn prepare(
        self,
        entry: RetainedEntry,
        bound_manifest_digest: [u8; 32],
        requirement: NativeLifetimeRequirement,
        executable_qualification: Option<CompiledArtifactQualification>,
    ) -> Result<PreparedNativeActivation, NativePreparationFailure> {
        let preflight = (|| {
            validate_lifetime(requirement)?;
            if bound_manifest_digest == [0; 32]
                || bound_manifest_digest != self.session.binding().manifest_digest
            {
                return Err(invalid("definition manifest differs from bound session"));
            }
            self.revalidate()?;
            entry.revalidate()?;
            validate_compiled_binary(
                &entry.executable.file,
                &entry.executable.slot,
                executable_qualification.as_ref(),
                &self.sdk,
            )
        })();
        // The admission is a validation gate only; nothing retains its facts.
        if let Err(error) = preflight {
            return Err(NativePreparationFailure {
                error,
                native_error: None,
                retained_broker: Some(Box::new(self)),
                retained_entry: Some(Box::new(entry)),
                retained_objects: None,
            });
        }
        let objects = match sdk::Objects::prepare(
            &entry.bytes,
            &entry.definition.command,
            &self.environment,
            self.sdk.glib_version,
            self.sdk_images.clone(),
        ) {
            Ok(objects) => objects,
            Err(mut failure) => {
                failure.retained_broker = Some(Box::new(self));
                failure.retained_entry = Some(Box::new(entry));
                return Err(failure);
            }
        };
        let original_session = self.session.binding().clone();
        Ok(PreparedNativeActivation {
            broker: Some(self),
            original_session,
            entry,
            objects,
            requirement,
        })
    }
}

pub struct NativePreparationFailure {
    pub error: io::Error,
    pub native_error: Option<sdk::NativeErrorOwner>,
    retained_broker: Option<Box<DedicatedGioBroker>>,
    retained_entry: Option<Box<RetainedEntry>>,
    retained_objects: Option<Box<sdk::Objects>>,
}
impl std::fmt::Debug for NativePreparationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativePreparationFailure")
            .field("error_kind", &self.error.kind())
            .field(
                "retained_original",
                &(self.retained_broker.is_some() || self.retained_objects.is_some()),
            )
            .finish_non_exhaustive()
    }
}
impl NativePreparationFailure {
    pub fn observe_session(&mut self, observed: Option<&SessionBinding>) -> io::Result<()> {
        self.retained_broker
            .as_mut()
            .ok_or_else(|| invalid("original failed preparation broker unavailable"))?
            .observe_session(observed)
    }
}
impl Drop for NativePreparationFailure {
    fn drop(&mut self) {
        // Partial SDK setup may already own an original bus/context or global
        // mapping. Failure is Unknown, never no-effect or a new admission right.
        std::mem::forget(self.retained_objects.take());
        std::mem::forget(self.retained_broker.take());
        std::mem::forget(self.retained_entry.take());
        std::mem::forget(self.native_error.take());
    }
}
/// Native parser/context owners, retained on the original worker thread.
pub struct PreparedNativeActivation {
    broker: Option<DedicatedGioBroker>,
    original_session: SessionBinding,
    entry: RetainedEntry,
    objects: sdk::Objects,
    requirement: NativeLifetimeRequirement,
}
impl PreparedNativeActivation {
    pub fn observe_session(&mut self, observed: Option<&SessionBinding>) -> io::Result<()> {
        match &mut self.broker {
            Some(original) => original.observe_session(observed),
            None if observed == Some(&self.original_session) => Ok(()),
            None => Err(invalid("handed-back original session differs")),
        }
    }
    /// Consuming invocation prevents replay. Original operation acceptance must
    /// already be durable. The synchronous SDK call belongs to the dedicated
    /// broker worker; its transport deadline cannot cancel/drop native storage.
    pub fn activate(self) -> Result<NativeActivation, NativeStartFailure> {
        if let Err(error) = self
            .broker
            .as_ref()
            .ok_or_else(|| invalid("original GIO broker token absent"))
            .and_then(DedicatedGioBroker::revalidate)
            .and_then(|()| self.entry.revalidate())
        {
            return Err(NativeStartFailure {
                error,
                prepared: Box::new(self),
            });
        }
        if let Err(error) = require_no_existing_children() {
            return Err(NativeStartFailure {
                error,
                prepared: Box::new(self),
            });
        }
        let invocation = OriginalInvocation {
            process: match &self.broker {
                Some(broker) => broker.process,
                None => {
                    return Err(NativeStartFailure {
                        error: invalid("original GIO broker token absent"),
                        prepared: Box::new(self),
                    });
                }
            },
        };
        let mut owner = NativeActivation {
            prepared: Some(self),
            receipt: NativeLaunchReceipt::Ambiguous,
            errors: Vec::new(),
            children: Vec::with_capacity(MAX_CALLBACKS),
            unknown: false,
            errors_truncated: false,
            primary_reaped: false,
            local_retired: false,
        };
        let prepared = owner.prepared.as_mut().expect("original prepared owner");
        owner.receipt = prepared
            .objects
            .launch(prepared.requirement == NativeLifetimeRequirement::OwnedPrimary);
        if prepared.requirement == NativeLifetimeRequirement::OwnedPrimary {
            for &pid in prepared.objects.callback_pids() {
                if pid <= 0 {
                    owner.unknown = true;
                    continue;
                }
                if !owner.children.iter().any(|child| child.pid == pid) {
                    owner
                        .children
                        .push(OriginalGioChild::retain_callback(&invocation, pid));
                }
            }
            if prepared.objects.callback_overflow()
                || owner.children.len() != 1
                || !matches!(
                    owner.receipt,
                    NativeLaunchReceipt::AcknowledgedSpawnCallback { .. }
                )
            {
                owner.unknown = true;
            }
        }
        if let Err(error) = prepared.entry.revalidate().and_then(|()| {
            prepared
                .broker
                .as_ref()
                .ok_or_else(|| invalid("original GIO broker token absent"))
                .and_then(DedicatedGioBroker::revalidate)
        }) {
            owner.errors.push(error);
            owner.receipt = NativeLaunchReceipt::Ambiguous;
            owner.unknown = true;
        }
        Ok(owner)
    }
}
pub struct NativeStartFailure {
    pub error: io::Error,
    pub prepared: Box<PreparedNativeActivation>,
}
impl std::fmt::Debug for NativeStartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeStartFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}
fn native_generation_handoff_allowed(
    primary_reaped: bool,
    local_retired: bool,
    unknown: bool,
    errors: bool,
    owned_primary: bool,
) -> bool {
    primary_reaped && local_retired && !unknown && !errors && owned_primary
}

/// Original qualified invocation, reserved child and native storage. Callback
/// numbers alone confer no ownership. There is no import, signal or tree claim.
pub struct NativeActivation {
    prepared: Option<PreparedNativeActivation>,
    receipt: NativeLaunchReceipt,
    errors: Vec<io::Error>,
    children: Vec<OriginalGioChild>,
    unknown: bool,
    errors_truncated: bool,
    primary_reaped: bool,
    local_retired: bool,
}
impl NativeActivation {
    pub fn retirement_native_errors(&self) -> &[sdk::NativeErrorOwner] {
        self.prepared
            .as_ref()
            .map(|p| p.objects.retirement_errors())
            .unwrap_or(&[])
    }
    pub fn retained_errors(&self) -> &[io::Error] {
        &self.errors
    }
    /// Sticky uncertainty from the caller's original launch deadline/cancel or
    /// lost native context. It never grants a new launch or retirement claim.
    pub fn quarantine(&mut self) {
        self.unknown = true;
    }
    pub fn owned_primary_available(&self) -> bool {
        !self.unknown && self.children.len() == 1
    }
    pub fn errors_truncated(&self) -> bool {
        self.errors_truncated
            || self
                .prepared
                .as_ref()
                .is_some_and(|p| p.objects.retirement_errors_truncated())
    }
    fn remember(&mut self, error: io::Error) {
        self.unknown = true;
        if self.errors.len() < 16 {
            self.errors.push(error);
        } else {
            self.errors_truncated = true;
        }
    }
    pub fn poll_primary(&mut self) -> io::Result<NativePrimaryState> {
        if self.primary_reaped {
            return Ok(if self.unknown {
                NativePrimaryState::Unknown
            } else {
                NativePrimaryState::Exited
            });
        }
        if self.prepared.as_ref().expect("original owner").requirement
            != NativeLifetimeRequirement::OwnedPrimary
        {
            return Ok(NativePrimaryState::Unavailable);
        }
        for index in 0..self.children.len() {
            if let Err(error) = self.children[index].poll() {
                self.remember(error);
                return Err(io::Error::other(
                    "original child wait failed; native error retained",
                ));
            }
        }
        if self.unknown || self.children.len() != 1 {
            return Ok(NativePrimaryState::Unknown);
        }
        Ok(match self.children[0].state {
            ChildReservation::Running => NativePrimaryState::Running,
            ChildReservation::Exited { .. } | ChildReservation::Reaped => {
                NativePrimaryState::Exited
            }
            ChildReservation::Unknown => NativePrimaryState::Unknown,
        })
    }
    /// Correlation only while this exact original child is still reserved.
    /// Never import, signal or reconstruct an owner from this number.
    pub fn original_peer_pid(&mut self) -> io::Result<u32> {
        if self.primary_reaped || !self.owned_primary_available() {
            return Err(invalid("original native primary correlation unavailable"));
        }
        match self.poll_primary()? {
            NativePrimaryState::Running | NativePrimaryState::Exited => {
                Ok(self.children[0].pid as u32)
            }
            _ => Err(invalid("original native child reservation unproved")),
        }
    }
    /// # Safety
    /// Router must have actually retired BOTH exact original READY+CLOSE
    /// LocalChallenge owners and any other original native IPC. No future peer
    /// authentication may use this child. Preserve the dedicated process,
    /// no-other-reaper and default-SIGCHLD contract until final settlement.
    pub unsafe fn retire_primary_after_ipc(&mut self) -> io::Result<bool> {
        if self.primary_reaped {
            return Ok(!self.unknown);
        }
        if self.poll_primary()? != NativePrimaryState::Exited {
            return Ok(false);
        }
        for index in 0..self.children.len() {
            match self.children[index].reap() {
                Ok(true) => {}
                Ok(false) => return Ok(false),
                Err(error) => {
                    self.remember(error);
                    return Err(io::Error::other(
                        "original child reap failed; native error retained",
                    ));
                }
            }
        }
        self.primary_reaped = true;
        Ok(!self.unknown)
    }
    pub fn poll_local_retirement(&mut self) -> io::Result<bool> {
        if self
            .prepared
            .as_ref()
            .expect("original owner")
            .objects
            .retirement_unknown()
        {
            self.quarantine();
            return Ok(false);
        }
        if self.local_retired {
            return Ok(true);
        }
        if self.unknown {
            return Ok(false);
        }
        if self.prepared.as_ref().expect("original owner").requirement
            == NativeLifetimeRequirement::OwnedPrimary
            && !self.primary_reaped
        {
            return Ok(false);
        }
        let result = self
            .prepared
            .as_mut()
            .expect("original native owner")
            .objects
            .poll_retirement();
        match result {
            Ok(retired) => {
                self.local_retired = retired;
                Ok(retired)
            }
            Err(error) => {
                self.remember(error);
                Err(io::Error::other(
                    "native retirement failed; original owners retained",
                ))
            }
        }
    }
    pub fn local_retired(&self) -> bool {
        self.local_retired
            && !self.unknown
            && !self
                .prepared
                .as_ref()
                .expect("original owner")
                .objects
                .retirement_unknown()
    }
    /// Transfer the SAME dedicated SDK/session token only after actual original
    /// primary reap and complete original SDK/IPC retirement. No admission or
    /// SDK initialization is repeated. Original observations remain in this owner.
    pub fn take_settled_broker(&mut self) -> io::Result<DedicatedGioBroker> {
        let errors = !self.errors.is_empty()
            || self.errors_truncated()
            || !self.retirement_native_errors().is_empty();
        if !native_generation_handoff_allowed(
            self.primary_reaped,
            self.local_retired(),
            self.unknown,
            errors,
            self.prepared
                .as_ref()
                .is_some_and(|p| p.requirement == NativeLifetimeRequirement::OwnedPrimary),
        ) {
            return Err(invalid("original native generation remains unresolved"));
        }
        let prepared = self
            .prepared
            .as_mut()
            .ok_or_else(|| invalid("original native storage unavailable"))?;
        let broker = prepared
            .broker
            .as_mut()
            .ok_or_else(|| invalid("original native token already handed back"))?;
        if let Err(error) = broker
            .revalidate()
            .and_then(|()| require_no_existing_children())
        {
            // Do not erase cached original retirement facts; revoke only the
            // transferable token so a later restored namespace/child cannot rearm it.
            broker.revoked = true;
            return Err(error);
        }
        prepared
            .broker
            .take()
            .ok_or_else(|| invalid("original native token already handed back"))
    }

    pub fn observe_session(&mut self, observed: Option<&SessionBinding>) -> io::Result<()> {
        self.prepared
            .as_mut()
            .expect("retained original owner")
            .observe_session(observed)
    }
}
impl Drop for NativeActivation {
    fn drop(&mut self) {
        // GIO can have pending bus flush/native lifetime after its synchronous
        // launch acknowledgement. Without an original completion right, keep all
        // context/object/callback/error/library owners until dedicated process
        // exit. Provider disposal never changes this historical uncertainty.
        if !self.local_retired
            || self.unknown
            || self
                .prepared
                .as_ref()
                .is_some_and(|p| p.objects.retirement_unknown())
        {
            std::mem::forget(self.prepared.take());
            std::mem::forget(std::mem::take(&mut self.children));
            std::mem::forget(std::mem::take(&mut self.errors));
        }
    }
}
#[cfg(test)]
mod tests;
