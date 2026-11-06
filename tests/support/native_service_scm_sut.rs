//! Installer-only SCM registration/configuration/removal. Never a helper API.
#![cfg(windows)]
use super::acl;
use super::definition::{self, ScmReaderConfig};
use anyhow::{Context, Result, ensure};
use std::{
    ffi::c_void,
    fs::File,
    io::{self, Read, Write},
    os::windows::{
        fs::MetadataExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Path, PathBuf},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use theseus_host::{
    fs::{HostDir, LeafOpenResult, RootPolicy, fingerprint},
    services::scm as host,
};
use theseus_native_protocol::v2;
type Handle = *mut c_void;
#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut c_void,
    inherit: i32,
}
#[repr(C)]
#[derive(Default)]
struct Status {
    kind: u32,
    state: u32,
    controls: u32,
    exit: u32,
    specific: u32,
    checkpoint: u32,
    wait_hint: u32,
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenSCManagerW(machine: *const u16, database: *const u16, access: u32) -> Handle;
    fn CreateServiceW(
        manager: Handle,
        name: *const u16,
        display: *const u16,
        access: u32,
        kind: u32,
        start: u32,
        error: u32,
        command: *const u16,
        group: *const u16,
        tag: *mut u32,
        dependencies: *const u16,
        account: *const u16,
        password: *const u16,
    ) -> Handle;
    fn ChangeServiceConfigW(
        service: Handle,
        kind: u32,
        start: u32,
        error: u32,
        command: *const u16,
        group: *const u16,
        tag: *mut u32,
        dependencies: *const u16,
        account: *const u16,
        password: *const u16,
        display: *const u16,
    ) -> i32;
    fn StartServiceW(service: Handle, count: u32, arguments: *const *const u16) -> i32;
    fn DeleteService(service: Handle) -> i32;
    fn ControlService(service: Handle, control: u32, status: *mut Status) -> i32;
    fn QueryServiceStatus(service: Handle, status: *mut Status) -> i32;
    fn CloseServiceHandle(handle: Handle) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const SecurityAttributes,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn SetFileInformationByHandle(
        file: Handle,
        class: i32,
        info: *const c_void,
        length: u32,
    ) -> i32;
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn terminated(s: &[u16]) -> Vec<u16> {
    s.iter().copied().chain(Some(0)).collect()
}
fn invalid(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, s)
}
fn budget(deadline: Instant, cancel: &AtomicBool) -> Result<()> {
    ensure!(
        !cancel.load(Ordering::Acquire)
            && Instant::now() < deadline
            && deadline.saturating_duration_since(Instant::now()) <= Duration::from_secs(30),
        "fixture SCM deadline/cancellation"
    );
    Ok(())
}
struct ServiceHandle(Handle);
impl ServiceHandle {
    fn close(&mut self) -> io::Result<()> {
        if !self.0.is_null() {
            if unsafe { CloseServiceHandle(self.0) } == 0 {
                return Err(io::Error::last_os_error());
            }
            self.0 = ptr::null_mut();
        }
        Ok(())
    }
    fn checked(raw: Handle) -> io::Result<Self> {
        if raw.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(raw))
        }
    }
}
impl Drop for ServiceHandle {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

pub struct NativeServiceSut {
    _manager: ServiceHandle,
    observer: host::ScmManager,
    name: Vec<u16>,
    definitions: Vec<v2::ServiceDefinition>,
    sidecar: PathBuf,
    input: ScmReaderConfig,
    retained_service: Option<ServiceHandle>,
    uncertain: bool,
    transitioning: bool,
}
impl NativeServiceSut {
    pub fn connect(
        manifest: &v2::IntegrationManifest,
        context: &v2::IntegrationRuntimeContext,
        fault: &str,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        manifest.validate_context(context, v2::Os::Windows)?;
        let v2::ServiceAuthority::Scm {
            service_name,
            qualification: q,
            preflight: p,
            ..
        } = manifest
            .service_authority
            .as_ref()
            .context("service authority missing")?
        else {
            anyhow::bail!("SCM authority required")
        };
        budget(deadline, cancel)?;
        host::verify_platform_version(&q.version)?;
        definition::verify_current_sid(&p.helper_sid)?;
        let readiness = manifest
            .readiness
            .as_ref()
            .ok_or_else(|| invalid("SCM readiness required"))?;
        let input = ScmReaderConfig {
            kind: "theseus_native_scm_reader".into(),
            schema: 1,
            service_name: service_name.clone(),
            endpoint: p.pipe.clone(),
            data: p.data.clone(),
            cooperative_instance: readiness
                .cooperative_instance
                .clone()
                .ok_or_else(|| invalid("original installation instance required"))?,
            fault: fault.into(),
            helper_sid: p.helper_sid.clone(),
            service_sid: p.configured_account_sid.clone(),
        };
        input.validate()?;
        let sidecar = Path::new(&p.executable).with_extension("service.json");
        ensure!(
            sidecar == Path::new(&p.definition),
            "SCM exact preflight definition must be immutable reader sidecar"
        );
        let authority = host::ScmAuthority {
            context: host::ScmContext::LocalMachine,
            service_name: service_name.clone(),
            ownership_id: context.ownership_id.clone(),
            context_receipt: context
                .namespace_receipt
                .clone()
                .context("service namespace receipt missing")?,
            qualification_receipt: q.receipt_sha256.clone(),
            allowed_access: 0x35,
            allowed_start: true,
            allowed_stop: true,
            allow_local_system: true,
            preflight: host::ScmPreflight {
                helper_sid: p.helper_sid.clone(),
                configured_account_sid: p.configured_account_sid.clone(),
                executable: p.executable.encode_utf16().collect(),
                definition: p.definition.encode_utf16().collect(),
                data: p.data.encode_utf16().collect(),
                pipe: p.pipe.encode_utf16().collect(),
                executable_and_definition_read_execute: p.executable_and_definition_read_execute,
                data_declared_modify_only: p.data_declared_modify_only,
                pipe_system_and_helper_only: p.pipe_system_and_helper_only,
                pipe_rejects_remote_clients: p.pipe_rejects_remote_clients,
                no_other_acl_grants: p.no_other_acl_grants,
                exact_access_receipt: p.exact_access_receipt.clone(),
            },
        };
        let observer = host::ScmManager::connect(authority, deadline, cancel)?;
        // CONNECT only until an explicit install needs CREATE_SERVICE. Never
        // ALL_ACCESS, remote, enumeration, database locking or account discovery.
        let manager =
            ServiceHandle::checked(unsafe { OpenSCManagerW(ptr::null(), ptr::null(), 1) })?;
        Ok(Self {
            _manager: manager,
            observer,
            name: wide(service_name),
            definitions: manifest.definitions.clone(),
            sidecar,
            input,
            retained_service: None,
            uncertain: false,
            transitioning: false,
        })
    }
    fn declared(&self, d: &v2::ServiceDefinition) -> Result<&v2::ScmDefinition> {
        let d = self
            .definitions
            .iter()
            .find(|candidate| *candidate == d)
            .ok_or_else(|| invalid("undeclared fixture SCM definition"))?;
        d.scm
            .as_ref()
            .ok_or_else(|| invalid("SCM next-start configuration required").into())
    }
    fn inspect(
        &mut self,
        expected: Option<&v2::ScmDefinition>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<()> {
        budget(deadline, cancel)?;
        let observation = self.observer.inspect(deadline, cancel)?;
        if let Some(expected) = expected {
            ensure!(
                observation.registration == host::RegistrationState::Present
                    && observation.next_start.as_ref() == Some(&raw(expected)),
                "fixture complete SCM next-start configuration differs"
            );
        } else {
            ensure!(
                observation.registration == host::RegistrationState::Absent,
                "fixture name absence not settled (delete-pending is not absent)"
            );
        }
        Ok(())
    }
    fn open_original(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
        ensure!(
            self.retained_service.is_some(),
            "original CreateServiceW handle missing; never reopen by matching name"
        );
        let observation = self.observer.inspect(deadline, cancel)?;
        ensure!(
            observation.registration == host::RegistrationState::Present
                && self
                    .definitions
                    .iter()
                    .filter_map(|d| d.scm.as_ref())
                    .any(|d| observation.next_start.as_ref() == Some(&raw(d))),
            "foreign current SCM registration cannot be adopted"
        );
        self.validate_sidecar()?;
        Ok(())
    }
    fn validate_sidecar(&self) -> Result<File> {
        let parent = HostDir::open(
            self.sidecar
                .parent()
                .ok_or_else(|| invalid("sidecar parent"))?,
            RootPolicy::AllComponentsNoFollow,
        )?;
        let mut file = match parent.open_leaf_classified(
            self.sidecar
                .file_name()
                .ok_or_else(|| invalid("sidecar name"))?,
        ) {
            LeafOpenResult::Opened(file) => file,
            other => anyhow::bail!("original SCM sidecar unavailable: {other:?}"),
        };
        let original = fingerprint(&file)?;
        ensure!(
            original.len > 0 && original.len <= 4096 && file.metadata()?.permissions().readonly(),
            "bounded readonly original SCM sidecar required"
        );
        let mut bytes = Vec::new();
        (&mut file).take(4097).read_to_end(&mut bytes)?;
        ensure!(
            fingerprint(&file)? == original && bytes == serde_json::to_vec(&self.input)?,
            "original SCM sidecar input differs"
        );
        Ok(file)
    }
    fn create_sidecar(&self) -> Result<File> {
        struct Descriptor(*mut c_void);
        impl Drop for Descriptor {
            fn drop(&mut self) {
                unsafe { LocalFree(self.0) };
            }
        }
        let sddl = wide(&format!(
            "D:P(A;;FR;;;SY)(A;;0x00130189;;;{})",
            self.input.helper_sid
        ));
        let mut descriptor = ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        let descriptor = Descriptor(descriptor);
        let security = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            descriptor: descriptor.0,
            inherit: 0,
        };
        let path = wide(
            self.sidecar
                .to_str()
                .ok_or_else(|| invalid("sidecar UTF8"))?,
        );
        // CREATE_NEW and no inheritance: never rewrite an existing config. The
        // provider's exact helper and SYSTEM are the only read ACEs. The creation
        // handle writes once, then only a readonly original handle is retained.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                0x40000000,
                1,
                &security,
                1,
                0x00200000,
                ptr::null_mut(),
            )
        };
        if handle as isize == -1 {
            return Err(io::Error::last_os_error().into());
        }
        let mut file = unsafe { File::from_raw_handle(handle) };
        let bytes = serde_json::to_vec(&self.input)?;
        ensure!(bytes.len() <= 4096, "SCM sidecar byte bound");
        file.write_all(&bytes)?;
        file.sync_all()?;
        let mut permissions = file.metadata()?.permissions();
        permissions.set_readonly(true);
        file.set_permissions(permissions)?;
        drop(file);
        acl::verify_path(&self.sidecar, &self.input.helper_sid, acl::Kind::Sidecar)?;
        self.validate_sidecar()
    }
    pub fn install(
        &mut self,
        definition: &v2::ServiceDefinition,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<()> {
        ensure!(
            !self.uncertain,
            "original fixture SCM mutation unresolved; no replay"
        );
        let d = self.declared(definition)?.clone();
        self.inspect(None, deadline, cancel)?;
        // Install only: the input is created once before CreateService and Start.
        let _input = self.create_sidecar()?;
        let reader = self
            .sidecar
            .parent()
            .ok_or_else(|| invalid("reader parent"))?
            .join("reader.exe");
        // CopyFile security is never accepted by inheritance or receipt flags.
        // This exact newly-created SUT object receives its declared initial ACL.
        acl::initialize_created_path(&reader, &self.input.helper_sid, acl::Kind::Executable)?;
        acl::verify_path(&reader, &self.input.helper_sid, acl::Kind::Executable)?;
        acl::verify_path(&self.sidecar, &self.input.helper_sid, acl::Kind::Sidecar)?;
        let command = terminated(&d.command_line);
        let display = terminated(&d.display_name);
        let account = terminated(&d.account);
        budget(deadline, cancel)?;
        let creator =
            ServiceHandle::checked(unsafe { OpenSCManagerW(ptr::null(), ptr::null(), 3) })?;
        self.uncertain = true;
        let handle = unsafe {
            CreateServiceW(
                creator.0,
                self.name.as_ptr(),
                display.as_ptr(),
                0x10037,
                d.service_type,
                d.start_type,
                d.error_control,
                command.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null(),
                account.as_ptr(),
                ptr::null(),
            )
        };
        self.retained_service = Some(ServiceHandle::checked(handle)?);
        self.inspect(Some(&d), deadline, cancel)?;
        self.uncertain = false;
        Ok(())
    }
    pub fn has_owned_registration(&self) -> bool {
        self.retained_service.is_some()
    }
    pub fn configure(
        &mut self,
        definition: &v2::ServiceDefinition,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<()> {
        ensure!(
            !self.uncertain,
            "original fixture SCM mutation unresolved; no replay"
        );
        let d = self.declared(definition)?.clone();
        if !self.transitioning {
            self.open_original(deadline, cancel)?;
        }
        let _input = self.validate_sidecar()?;
        let reader = self
            .sidecar
            .parent()
            .ok_or_else(|| invalid("reader parent"))?
            .join("reader.exe");
        if self.transitioning {
            // The stopped original was removed by verified handle; this is the
            // newly-created pinned target object, never an overwrite grant.
            acl::initialize_created_path(&reader, &self.input.helper_sid, acl::Kind::Executable)?;
        }
        acl::verify_path(&reader, &self.input.helper_sid, acl::Kind::Executable)?;
        acl::verify_path(&self.sidecar, &self.input.helper_sid, acl::Kind::Sidecar)?;
        if !self.transitioning {
            acl::initialize_created_path(
                Path::new(&self.input.data),
                &self.input.helper_sid,
                acl::Kind::Data,
            )?;
        }
        acl::verify_path(
            Path::new(&self.input.data),
            &self.input.helper_sid,
            acl::Kind::Data,
        )?;
        let command = terminated(&d.command_line);
        let display = terminated(&d.display_name);
        let account = terminated(&d.account);
        budget(deadline, cancel)?;
        self.uncertain = true;
        if unsafe {
            ChangeServiceConfigW(
                self.retained_service.as_ref().unwrap().0,
                d.service_type,
                d.start_type,
                d.error_control,
                command.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null(),
                account.as_ptr(),
                ptr::null(),
                display.as_ptr(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        self.inspect(Some(&d), deadline, cancel)?;
        self.uncertain = false;
        Ok(())
    }
    /// Hold the SAME original SUT service handle through stop/config/restart.
    /// No repeat CreateService/DeleteService or reconstructed registration occurs.
    pub fn stop_for_transition(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
        ensure!(
            !self.uncertain && !self.transitioning,
            "original SCM SUT transition unresolved"
        );
        self.open_original(deadline, cancel)?;
        self.uncertain = true;
        let handle = self.retained_service.as_ref().unwrap().0;
        let mut status = Status::default();
        if unsafe { QueryServiceStatus(handle, &mut status) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        ensure!(status.kind == 0x10, "original SCM service type changed");
        if status.state != 1
            && status.state != 3
            && unsafe { ControlService(handle, 1, &mut status) } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        loop {
            budget(deadline, cancel)?;
            if unsafe { QueryServiceStatus(handle, &mut status) } == 0 {
                return Err(io::Error::last_os_error().into());
            }
            ensure!(
                status.kind == 0x10,
                "original SCM service type changed while stopping"
            );
            if status.state == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        self.transitioning = true;
        self.uncertain = false;
        Ok(())
    }
    pub fn start_after_transition(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
        ensure!(
            !self.uncertain && self.transitioning && self.retained_service.is_some(),
            "original stopped SCM SUT handle required"
        );
        budget(deadline, cancel)?;
        self.uncertain = true;
        let handle = self.retained_service.as_ref().unwrap().0;
        if unsafe { StartServiceW(handle, 0, ptr::null()) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        loop {
            budget(deadline, cancel)?;
            let mut status = Status::default();
            if unsafe { QueryServiceStatus(handle, &mut status) } == 0 {
                return Err(io::Error::last_os_error().into());
            }
            ensure!(
                status.kind == 0x10,
                "original SCM service type changed while starting"
            );
            if status.state == 4 {
                break;
            }
            ensure!(status.state == 2, "original SUT service failed to start");
            std::thread::sleep(Duration::from_millis(25));
        }
        self.transitioning = false;
        self.uncertain = false;
        Ok(())
    }
    pub fn uninstall(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
        ensure!(
            !self.uncertain,
            "original fixture SCM mutation unresolved; no replay"
        );
        self.open_original(deadline, cancel)?;
        let _input = self.validate_sidecar()?;
        self.uncertain = true;
        let handle = self.retained_service.as_ref().unwrap().0;
        let mut status = Status::default();
        if unsafe { QueryServiceStatus(handle, &mut status) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        if status.state != 1
            && status.state != 3
            && unsafe { ControlService(handle, 1, &mut status) } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        loop {
            budget(deadline, cancel)?;
            if unsafe { QueryServiceStatus(handle, &mut status) } == 0 {
                return Err(io::Error::last_os_error().into());
            }
            ensure!(
                status.kind == 0x10,
                "original SCM fixture service type changed"
            );
            if status.state == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        budget(deadline, cancel)?;
        if unsafe { DeleteService(handle) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        // Success accepts deletion of the original object. It deliberately does
        // not claim name absence: helper/extra original handles can keep pending
        // deletion alive. The shared product observes closure and absence later.
        if let Some(original) = self.retained_service.as_mut() {
            original.close()?;
        }
        self.retained_service.take();
        self.uncertain = false;
        Ok(())
    }
    /// Only after successful original DeleteService acceptance. File removal is
    /// a SUT uninstall effect and never substitutes for manager name absence.
    pub fn remove_sidecar(&self) -> Result<()> {
        let file = self.validate_sidecar()?;
        let parent = HostDir::open(
            self.sidecar
                .parent()
                .ok_or_else(|| invalid("sidecar parent"))?,
            RootPolicy::AllComponentsNoFollow,
        )?;
        clear_readonly_original(&self.sidecar, &file)?;
        parent.remove_created_leaf(
            self.sidecar
                .file_name()
                .ok_or_else(|| invalid("sidecar name"))?,
            &file,
        )?;
        Ok(())
    }
}
impl Drop for NativeServiceSut {
    fn drop(&mut self) {
        if self.uncertain
            && let Some(handle) = self.retained_service.take()
        {
            std::mem::forget(handle);
        }
    }
}
fn raw(d: &v2::ScmDefinition) -> host::ScmDefinition {
    host::ScmDefinition {
        command_line: d.command_line.clone(),
        display_name: d.display_name.clone(),
        account: d.account.clone(),
        service_type: d.service_type,
        start_type: d.start_type,
        error_control: d.error_control,
        load_order_group: d.load_order_group.clone(),
        dependencies: d.dependencies.clone(),
        tag_id: d.tag_id,
        delayed_auto_start: d.delayed_auto_start,
        failure_actions: d
            .failure_actions
            .iter()
            .map(|a| host::FailureAction {
                kind: a.kind,
                delay_ms: a.delay_ms,
            })
            .collect(),
        failure_reset_period: d.failure_reset_period,
        failure_reboot_message: d.failure_reboot_message.clone(),
        failure_command: d.failure_command.clone(),
        failure_actions_on_non_crash: d.failure_actions_on_non_crash,
        service_sid_type: d.service_sid_type,
        required_privileges: d.required_privileges.clone(),
        triggers_present: d.triggers_present,
        launch_protected: d.launch_protected,
    }
}

// FILE_BASIC_INFO is fixed SDK layout; zero timestamps preserve existing values.
#[repr(C)]
struct BasicInfo {
    created: i64,
    accessed: i64,
    written: i64,
    changed: i64,
    attributes: u32,
}
fn clear_readonly_original(path: &Path, original: &File) -> Result<()> {
    let name = wide(path.to_str().ok_or_else(|| invalid("fixture path UTF8"))?);
    // Only attribute-write/read access, no content-write. Reparse leaves are
    // opened themselves then rejected by fingerprint. All mutation uses the
    // verified original HANDLE, so a path replacement stays untouched.
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            0x00100181,
            7,
            ptr::null(),
            3,
            0x00200000,
            ptr::null_mut(),
        )
    };
    if raw as isize == -1 {
        return Err(io::Error::last_os_error().into());
    }
    let writable = unsafe { File::from_raw_handle(raw) };
    ensure!(
        fingerprint(&writable)? == fingerprint(original)?,
        "original fixture file changed before readonly removal"
    );
    let attributes = writable.metadata()?.file_attributes();
    if attributes & 1 != 0 {
        let info = BasicInfo {
            created: 0,
            accessed: 0,
            written: 0,
            changed: 0,
            attributes: if attributes & !1 == 0 {
                0x80
            } else {
                attributes & !1
            },
        };
        if unsafe {
            SetFileInformationByHandle(
                writable.as_raw_handle(),
                0,
                (&info as *const BasicInfo).cast(),
                std::mem::size_of::<BasicInfo>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
    }
    Ok(())
}
/// Repeat-install reader owner pinned BEFORE native Stop. Contents alone never
/// authorize deleting an object opened later under the same path.
pub struct PinnedReader {
    root: HostDir,
    path: PathBuf,
    leaf: std::ffi::OsString,
    file: File,
    original: theseus_host::fs::FileFingerprint,
}
impl PinnedReader {
    pub fn pin(path: &Path, length: u64, sha256: &str) -> Result<Self> {
        use sha2::{Digest as _, Sha256};
        let root = HostDir::open(
            path.parent().ok_or_else(|| invalid("reader parent"))?,
            RootPolicy::AllComponentsNoFollow,
        )?;
        let leaf = path
            .file_name()
            .ok_or_else(|| invalid("reader name"))?
            .to_owned();
        let mut file = match root.open_leaf_classified(&leaf) {
            LeafOpenResult::Opened(file) => file,
            other => anyhow::bail!("original fixture reader unavailable: {other:?}"),
        };
        let original = fingerprint(&file)?;
        ensure!(
            original.len == length && length <= 64 * 1024 * 1024,
            "original reader length differs"
        );
        let mut hash = Sha256::new();
        let mut read = 0u64;
        let mut buffer = [0u8; 16384];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            read += count as u64;
            ensure!(read <= length, "original reader grew during pinning");
            hash.update(&buffer[..count]);
        }
        ensure!(
            read == length
                && format!("{:x}", hash.finalize()) == sha256
                && fingerprint(&file)? == original,
            "original reader changed while pinning"
        );
        Ok(Self {
            root,
            path: path.into(),
            leaf,
            file,
            original,
        })
    }
    pub fn remove(self) -> Result<()> {
        ensure!(
            fingerprint(&self.file)? == self.original,
            "original reader changed while Stop was pending"
        );
        clear_readonly_original(&self.path, &self.file)?;
        self.root.remove_created_leaf(&self.leaf, &self.file)?;
        Ok(())
    }
}

/// Remove only a workspace reader matching one originally pinned artifact.
/// Clearing readonly uses its original verified handle and cannot alter a
/// same-path replacement. The private parent remains the namespace authority.
pub fn remove_reader(path: &Path, artifacts: &[(u64, String)]) -> Result<()> {
    use sha2::{Digest as _, Sha256};
    let parent = HostDir::open(
        path.parent().ok_or_else(|| invalid("reader parent"))?,
        RootPolicy::AllComponentsNoFollow,
    )?;
    let name = path.file_name().ok_or_else(|| invalid("reader name"))?;
    let mut file = match parent.open_leaf_classified(name) {
        LeafOpenResult::Opened(file) => file,
        LeafOpenResult::MissingSettled => return Ok(()),
        other => anyhow::bail!("original fixture reader unavailable: {other:?}"),
    };
    let before = fingerprint(&file)?;
    ensure!(
        artifacts.iter().any(|(length, _)| *length == before.len),
        "foreign fixture reader length"
    );
    let mut hasher = Sha256::new();
    let mut read = 0u64;
    let mut buffer = [0u8; 16384];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        read = read
            .checked_add(count as u64)
            .ok_or_else(|| invalid("reader size overflow"))?;
        ensure!(read <= before.len, "reader grew while hashing");
        hasher.update(&buffer[..count]);
    }
    let digest = format!("{:x}", hasher.finalize());
    ensure!(
        read == before.len
            && fingerprint(&file)? == before
            && artifacts
                .iter()
                .any(|(length, sha)| *length == read && sha == &digest),
        "foreign or changed fixture reader commitment"
    );
    clear_readonly_original(path, &file)?;
    parent.remove_created_leaf(name, &file)?;
    Ok(())
}

#[cfg(test)]
mod repeat_reader_tests {
    use super::*;
    use sha2::{Digest as _, Sha256};
    #[test]
    fn windows_pre_stop_reader_pin_refuses_same_byte_foreign_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reader.exe");
        let data = root.path().join("data");
        std::fs::write(&path, b"pinned-reader").unwrap();
        std::fs::write(&data, b"configured-data").unwrap();
        let pin = PinnedReader::pin(
            &path,
            13,
            &format!("{:x}", Sha256::digest(b"pinned-reader")),
        )
        .unwrap();
        std::fs::rename(&path, root.path().join("owned-original.exe")).unwrap();
        std::fs::write(&path, b"pinned-reader").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();
        assert!(pin.remove().is_err());
        assert!(
            std::fs::metadata(&path).unwrap().permissions().readonly(),
            "foreign attributes untouched"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"pinned-reader");
        assert_eq!(std::fs::read(&data).unwrap(), b"configured-data");
    }
    #[test]
    fn windows_pre_stop_pin_removes_only_owned_readonly_reader_and_preserves_data() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reader.exe");
        let data = root.path().join("data");
        std::fs::write(&path, b"pinned-reader").unwrap();
        std::fs::write(&data, b"configured-data").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();
        let pin = PinnedReader::pin(
            &path,
            13,
            &format!("{:x}", Sha256::digest(b"pinned-reader")),
        )
        .unwrap();
        pin.remove().unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read(&data).unwrap(), b"configured-data");
    }
}
