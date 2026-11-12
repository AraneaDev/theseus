//! Exact Linux desktop-entry admission and native GIO activation.
//!
//! This is a mechanism for the already authenticated, durably accepted broker
//! operation. It never installs/repairs an entry or discovers a session. Native
//! launch acknowledgement alone supplies no owned-process or cleanup authority.
mod qualification;
mod sdk;
use crate::{
    desktop::session::{BoundSession, DesktopKind, SessionBinding, SuppliedDisplay},
    fs::{FileFingerprint, HostDir, RelativePath, RootPolicy, fingerprint, read_exact_at},
};
use qualification::validate_compiled_binary;
pub use qualification::{
    CompiledArtifactQualification, NativeBinaryAdmission, NativeElfAbi, ObservedNativeBinary,
    QualificationProvenance, QualifiedGioSdk, QualifiedSdkImage, QualifiedSdkImages, SdkImageRole,
};
pub use sdk::{DynamicLoaderFailure, NativeErrorOwner};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io,
    os::unix::{fs::PermissionsExt, io::AsRawFd},
    path::{Path, PathBuf},
};

const MAX_DEFINITION: u64 = 65_536;
const MAX_ARTIFACT: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiteralCommand {
    pub executable: String,
    pub arguments: Vec<String>,
}
/// Independently admitted manifest commitment, never populated from the entry.
#[derive(Clone, Debug)]
pub struct ArtifactSlot {
    pub root: PathBuf,
    pub relative: String,
    pub length: u64,
    pub sha256: [u8; 32],
}
#[derive(Clone, Debug)]
pub struct AdmittedDefinition {
    pub entry: ArtifactSlot,
    pub executable: ArtifactSlot,
    pub command: LiteralCommand,
}

/// Closed subset: no quoting, escaping or field expansion. The native parser
/// later reads these same bytes, and its executable/commandline must agree.
fn parse_entry(bytes: &[u8], expected: &LiteralCommand) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_DEFINITION as usize || bytes.contains(&0) {
        return Err(invalid("desktop entry outside byte bound"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("desktop entry is not UTF-8"))?;
    let mut fields = BTreeMap::new();
    let mut group = false;
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[Desktop Entry]" && !group && fields.is_empty() {
            group = true;
            continue;
        }
        if !group || line.starts_with('[') || line.len() > 8192 || line.contains('\r') {
            return Err(invalid("unsupported desktop group or line"));
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| invalid("invalid desktop field"))?;
        if !matches!(
            key,
            "Type" | "Name" | "Exec" | "Terminal" | "DBusActivatable"
        ) || fields.insert(key, value).is_some()
        {
            return Err(invalid("unknown or duplicate desktop field"));
        }
    }
    if fields.len() != 5
        || fields.get("Type") != Some(&"Application")
        || fields.get("Terminal") != Some(&"false")
        || fields.get("DBusActivatable") != Some(&"false")
        || !fields.get("Name").is_some_and(|v| {
            !v.is_empty() && v.len() <= 256 && !v.chars().any(char::is_control) && !v.contains('\\')
        })
    {
        return Err(invalid("unsupported desktop application policy"));
    }
    validate_command(expected)?;
    let exec = fields["Exec"];
    let tokens: Vec<_> = exec.split(' ').collect();
    if tokens.len() != expected.arguments.len() + 1
        || tokens.first() != Some(&expected.executable.as_str())
        || tokens[1..]
            .iter()
            .copied()
            .ne(expected.arguments.iter().map(String::as_str))
        || tokens.iter().any(|v| !literal(v))
    {
        return Err(invalid(
            "desktop Exec differs from admitted literal command",
        ));
    }
    Ok(())
}
fn literal(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.contains("://")
        && value
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || b"/_.-+=,@".contains(&v))
}
fn validate_command(command: &LiteralCommand) -> io::Result<()> {
    if !absolute(&command.executable)
        || !literal(&command.executable)
        || command.arguments.len() > 64
        || command.arguments.iter().any(|arg| !literal(arg))
        || command.arguments.iter().map(String::len).sum::<usize>() > 8192
    {
        return Err(invalid(
            "command must contain one absolute executable and bounded literal argv",
        ));
    }
    Ok(())
}
fn absolute(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 2048
        && !value.as_bytes().contains(&0)
        && (value == "/"
            || !value[1..]
                .split('/')
                .any(|v| v.is_empty() || matches!(v, "." | "..")))
}

struct RetainedArtifact {
    slot: ArtifactSlot,
    root: HostDir,
    relative: RelativePath,
    file: File,
    fingerprint: FileFingerprint,
    executable: bool,
}
impl RetainedArtifact {
    fn admit(slot: ArtifactSlot, max: u64, executable: bool) -> io::Result<Self> {
        if !slot.root.to_str().is_some_and(absolute)
            || slot.relative.len() > 2048
            || slot.length == 0
            || slot.length > max
            || slot.sha256 == [0; 32]
        {
            return Err(invalid("invalid admitted artifact slot"));
        }
        let relative = RelativePath::parse(&slot.relative)?;
        let root = HostDir::open(&slot.root, RootPolicy::AllComponentsNoFollow)?;
        let file = root.open_file(&relative)?;
        let fingerprint = fingerprint(&file)?;
        let retained = Self {
            slot,
            root,
            relative,
            file,
            fingerprint,
            executable,
        };
        retained.revalidate()?;
        Ok(retained)
    }
    fn path(&self) -> PathBuf {
        self.slot.root.join(&self.slot.relative)
    }
    fn revalidate(&self) -> io::Result<()> {
        let root = HostDir::open(&self.slot.root, RootPolicy::AllComponentsNoFollow)?;
        if !self.root.same_object(&root)? {
            return Err(invalid("original supplied artifact root replaced"));
        }
        let mapped = self.root.open_file(&self.relative)?;
        if fingerprint(&mapped)? != self.fingerprint || fingerprint(&self.file)? != self.fingerprint
        {
            return Err(invalid("original artifact file or mapping changed"));
        }
        if self.executable {
            let mode = self.file.metadata()?.permissions().mode();
            if mode & 0o111 == 0 || mode & 0o6000 != 0 {
                return Err(invalid("artifact is not a non-elevating executable"));
            }
            let size = unsafe {
                libc::fgetxattr(
                    self.file.as_raw_fd(),
                    c"security.capability".as_ptr(),
                    std::ptr::null_mut(),
                    0,
                )
            };
            if size > 0 {
                return Err(invalid("file capabilities are unsupported"));
            }
            if size < 0 {
                let error = io::Error::last_os_error();
                if !matches!(error.raw_os_error(), Some(libc::ENODATA | libc::ENOTSUP)) {
                    return Err(error);
                }
            }
        }
        if self.fingerprint.len != self.slot.length
            || hash_file(&self.file, self.slot.length)? != self.slot.sha256
        {
            return Err(invalid("artifact differs from supplied length/SHA256"));
        }
        if fingerprint(&self.file)? != self.fingerprint
            || fingerprint(&self.root.open_file(&self.relative)?)? != self.fingerprint
        {
            return Err(invalid("artifact changed during bounded hash"));
        }
        let root_after = HostDir::open(&self.slot.root, RootPolicy::AllComponentsNoFollow)?;
        if !self.root.same_object(&root_after)? {
            return Err(invalid("artifact root changed during read"));
        }
        Ok(())
    }
    fn bytes(&self) -> io::Result<Vec<u8>> {
        if self.slot.length > MAX_DEFINITION {
            return Err(invalid("definition exceeds bound"));
        }
        let mut bytes = vec![0; self.slot.length as usize];
        read_exact_at(&self.file, &mut bytes, 0)?;
        if hash_bytes(&bytes) != self.slot.sha256 {
            return Err(invalid("definition snapshot differs from supplied hash"));
        }
        self.revalidate()?;
        Ok(bytes)
    }
}
fn hash_file(file: &File, length: u64) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut buffer = [0; 32_768];
    let mut offset = 0;
    while offset < length {
        let count = (length - offset).min(buffer.len() as u64) as usize;
        read_exact_at(file, &mut buffer[..count], offset)?;
        hash.update(&buffer[..count]);
        offset += count as u64;
    }
    Ok(hash.finalize().into())
}
fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Original retained entry and executable. Neither can be rebuilt from receipt
/// bytes or a PID. Revalidation is sticky after any observed authority failure.
pub struct RetainedEntry {
    entry: RetainedArtifact,
    executable: RetainedArtifact,
    definition: AdmittedDefinition,
    bytes: Vec<u8>,
    revoked: std::cell::Cell<bool>,
}
impl RetainedEntry {
    pub fn admit(definition: AdmittedDefinition) -> io::Result<Self> {
        let entry = RetainedArtifact::admit(definition.entry.clone(), MAX_DEFINITION, false)?;
        if !entry.path().extension().is_some_and(|v| v == "desktop") {
            return Err(invalid("entry slot must be a desktop file"));
        }
        let executable =
            RetainedArtifact::admit(definition.executable.clone(), MAX_ARTIFACT, true)?;
        if executable.path() != Path::new(&definition.command.executable) {
            return Err(invalid("Exec path differs from admitted executable slot"));
        }
        let bytes = entry.bytes()?;
        parse_entry(&bytes, &definition.command)?;
        Ok(Self {
            entry,
            executable,
            definition,
            bytes,
            revoked: std::cell::Cell::new(false),
        })
    }
    pub fn revalidate(&self) -> io::Result<()> {
        if self.revoked.get() {
            return Err(invalid("original desktop artifact authority revoked"));
        }
        let result = self
            .entry
            .revalidate()
            .and_then(|()| self.executable.revalidate());
        if result.is_err() {
            self.revoked.set(true);
        }
        result
    }
}

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChildWait {
    Pending,
    Running,
    Exited { code: i32, status: i32 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChildReservation {
    Running,
    Exited { code: i32, status: i32 },
    Reaped,
    Unknown,
}
struct OriginalInvocation {
    process: libc::pid_t,
}
/// No public PID import, serialization, process open or signal authority.
struct OriginalGioChild {
    pid: libc::pid_t,
    process: libc::pid_t,
    state: ChildReservation,
}
impl OriginalGioChild {
    fn retain_callback(invocation: &OriginalInvocation, pid: libc::pid_t) -> Self {
        Self {
            pid,
            process: invocation.process,
            state: ChildReservation::Running,
        }
    }
    fn poll_with(
        &mut self,
        wait: impl FnOnce(libc::pid_t) -> io::Result<ChildWait>,
    ) -> io::Result<NativePrimaryState> {
        match self.state {
            ChildReservation::Reaped => return Ok(NativePrimaryState::Exited),
            ChildReservation::Unknown => return Ok(NativePrimaryState::Unknown),
            _ => {}
        }
        let previous = self.state;
        match wait(self.pid) {
            Ok(ChildWait::Pending) => Ok(if matches!(previous, ChildReservation::Exited { .. }) {
                NativePrimaryState::Exited
            } else {
                NativePrimaryState::Running
            }),
            Ok(ChildWait::Running) if matches!(previous, ChildReservation::Exited { .. }) => {
                self.state = ChildReservation::Unknown;
                Err(invalid("original reserved exit regressed"))
            }
            Ok(ChildWait::Running) => Ok(NativePrimaryState::Running),
            Ok(ChildWait::Exited { code, status }) if matches!(previous,ChildReservation::Exited {code:old,status:old_status} if old!=code||old_status!=status) =>
            {
                self.state = ChildReservation::Unknown;
                Err(invalid("original reserved exit identity/status changed"))
            }
            Ok(ChildWait::Exited { code, status }) => {
                self.state = ChildReservation::Exited { code, status };
                Ok(NativePrimaryState::Exited)
            }
            Err(error) => {
                self.state = ChildReservation::Unknown;
                Err(error)
            }
        }
    }
    fn poll(&mut self) -> io::Result<NativePrimaryState> {
        if let Err(error) = verify_wait_contract(self.process) {
            self.state = ChildReservation::Unknown;
            return Err(error);
        }
        self.poll_with(wait_reserved_child)
    }
    fn reap_with(
        &mut self,
        reap: impl FnOnce(libc::pid_t) -> io::Result<Option<i32>>,
    ) -> io::Result<bool> {
        let (code, expected) = match self.state {
            ChildReservation::Reaped => return Ok(true),
            ChildReservation::Exited { code, status } => (code, status),
            _ => return Ok(false),
        };
        let result = (|| {
            let Some(status) = reap(self.pid)? else {
                return Ok(false);
            };
            let same = if code == libc::CLD_EXITED {
                libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == expected
            } else {
                libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == expected
            };
            if !same {
                return Err(invalid(
                    "original GIO child reap differs from reserved exit",
                ));
            }
            Ok(true)
        })();
        match result {
            Ok(true) => self.state = ChildReservation::Reaped,
            Err(_) => self.state = ChildReservation::Unknown,
            _ => {}
        }
        result
    }
    fn reap(&mut self) -> io::Result<bool> {
        if let Err(error) = verify_wait_contract(self.process) {
            self.state = ChildReservation::Unknown;
            return Err(error);
        }
        self.reap_with(reap_reserved_child)
    }
}
// Private primitives accept only internal invocation-reserved children; no
// public ownership import exists. Source fixtures exercise syscall semantics
// with their separately held std::process::Child, never as GIO activation.
fn wait_reserved_child(pid: libc::pid_t) -> io::Result<ChildWait> {
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    if unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(ChildWait::Pending);
        }
        return Err(error);
    }
    let observed = unsafe { info.si_pid() };
    if observed == 0 {
        return Ok(ChildWait::Running);
    }
    if observed != pid
        || !matches!(
            info.si_code,
            libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED
        )
    {
        return Err(invalid("original GIO child wait identity/status unproved"));
    }
    Ok(ChildWait::Exited {
        code: info.si_code,
        status: unsafe { info.si_status() },
    })
}
fn reap_reserved_child(pid: libc::pid_t) -> io::Result<Option<i32>> {
    let mut status = 0;
    let observed = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    if observed < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(None);
        }
        return Err(error);
    }
    if observed == 0 {
        return Ok(None);
    }
    if observed != pid {
        return Err(invalid("original GIO child reap identity unproved"));
    }
    Ok(Some(status))
}
fn verify_wait_contract(process: libc::pid_t) -> io::Result<()> {
    if process != unsafe { libc::getpid() } {
        return Err(invalid("original GIO broker process changed"));
    }
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), action.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let action = unsafe { action.assume_init() };
    if action.sa_sigaction != libc::SIG_DFL || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
        return Err(invalid(
            "original GIO child requires unchanged default SIGCHLD",
        ));
    }
    Ok(())
}
fn require_no_existing_children() -> io::Result<()> {
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    if unsafe {
        libc::waitid(
            libc::P_ALL,
            0,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    } == 0
    {
        return Err(invalid(
            "GIO dedicated broker has an unrelated/preexisting child",
        ));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ECHILD) {
        Ok(())
    } else {
        Err(error)
    }
}

/// Exact replacement environment, including process-global SDK inputs. Nothing
/// is filled from the current display, default context, HOME or session bus.
fn validate_environment(
    session: &BoundSession,
    entries: &[(String, String)],
    helper: &str,
) -> io::Result<()> {
    if !absolute(helper) || entries.len() > 64 {
        return Err(invalid("invalid supplied GIO environment"));
    }
    let mut env = BTreeMap::new();
    let mut bytes = 0;
    for (key, value) in entries {
        bytes += key.len() + value.len();
        let allowed = matches!(
            key.as_str(),
            "DISPLAY"
                | "XAUTHORITY"
                | "DBUS_SESSION_BUS_ADDRESS"
                | "WAYLAND_DISPLAY"
                | "XDG_RUNTIME_DIR"
                | "GIO_LAUNCH_DESKTOP"
                | "LANG"
                | "LC_ALL"
        ) || key.starts_with("THESEUS_FIXTURE_");
        if !allowed
            || key.len() > 128
            || key.is_empty()
            || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || value.len() > 4096
            || value.as_bytes().contains(&0)
            || env.insert(key.as_str(), value.as_str()).is_some()
        {
            return Err(invalid(
                "unapproved/duplicate supplied environment material",
            ));
        }
    }
    if bytes > 16_384 || env.get("GIO_LAUNCH_DESKTOP") != Some(&helper) {
        return Err(invalid(
            "GIO helper environment differs from supplied artifact",
        ));
    }
    let equal = |key: &str, value: &str| env.get(key).is_some_and(|actual| *actual == value);
    let valid = match &session.binding().display {
        SuppliedDisplay::X11 {
            display,
            authority_file,
            bus,
        } => {
            equal("DISPLAY", display)
                && equal("XAUTHORITY", authority_file)
                && equal(
                    "DBUS_SESSION_BUS_ADDRESS",
                    &format!("unix:path={}", bus.as_str()),
                )
                && !env.contains_key("WAYLAND_DISPLAY")
                && !env.contains_key("XDG_RUNTIME_DIR")
        }
        SuppliedDisplay::Wayland {
            socket,
            runtime_directory,
            bus,
        } => {
            equal("WAYLAND_DISPLAY", socket.as_str())
                && equal("XDG_RUNTIME_DIR", runtime_directory)
                && equal(
                    "DBUS_SESSION_BUS_ADDRESS",
                    &format!("unix:path={}", bus.as_str()),
                )
                && !env.contains_key("DISPLAY")
                && !env.contains_key("XAUTHORITY")
        }
        _ => false,
    };
    if !valid {
        return Err(invalid(
            "environment differs from bound supplied Linux display/session bus",
        ));
    }
    Ok(())
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
    pub fn receipt(&self) -> NativeLaunchReceipt {
        self.receipt
    }
    pub fn native_error(&self) -> Option<&sdk::NativeErrorOwner> {
        self.prepared
            .as_ref()
            .and_then(|p| p.objects.error.as_ref())
    }
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
    pub fn primary_reaped(&self) -> bool {
        self.primary_reaped
    }

    pub fn cleanup_complete(&self) -> bool {
        false
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
