use super::{
    command::{CommandOwner, remaining},
    model::*,
    ownership::{RegistrationOwner, read_definition, verify_original, verify_original_observation},
    parser::normalize_print,
};
use crate::fs::{FileFingerprint, HostDir, RootPolicy, fingerprint};
use std::{fs::File, io, marker::PhantomData, rc::Rc, sync::atomic::AtomicBool, time::Instant};
/// Exact terminal status of the retained native launchctl process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandCompletion {
    Exited(i32),
    NoNormalExit,
    SpawnFailed,
}
impl CommandCompletion {
    pub fn succeeded(self) -> bool {
        self == Self::Exited(0)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnedActionProgress {
    Pending {
        job: Option<String>,
    },
    /// Terminal describes the original launchctl command, not a launchd job,
    /// readiness, stopped process tree, or service lifetime settlement.
    Terminal {
        job: String,
        action: ServiceAction,
        completion: CommandCompletion,
        result: String,
    },
    Unknown {
        job: Option<String>,
        reason: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceObservation {
    pub registration: Fact<bool>,
    pub definition_present: bool,
    pub definition_bytes: Option<Vec<u8>>,
    pub loaded: Fact<LoadedService>,
    pub evidence: CommandEvidence,
}
struct ExecutableOwner {
    root: HostDir,
    file: File,
    fingerprint: FileFingerprint,
}
struct PendingCommand {
    kind: LaunchctlCommand,
    id: String,
    command: Option<CommandOwner>,
    definition: Option<(File, FileFingerprint, Vec<u8>)>,
    _root: HostDir,
    _executable: File,
    progress: OwnedActionProgress,
}
/// Stays on the existing helper worker. It does not provision/discover domains,
/// bootstrap/bootout, reload, remove, attach to PIDs, or reconstruct ownership.
pub struct LaunchdManager {
    authority: LaunchdAuthority,
    root: HostDir,
    executable: ExecutableOwner,
    instance: [u8; 32],
    sequence: u64,
    revoked: bool,
    qualified: bool,
    pending: Option<PendingCommand>,
    _worker: PhantomData<Rc<()>>,
}
impl LaunchdManager {
    pub fn connect(
        authority: LaunchdAuthority,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        authority.validate()?;
        remaining(deadline, cancel)?;
        verify_platform(&authority)?;
        let root = HostDir::open(
            &authority.definition_root,
            RootPolicy::AllComponentsNoFollow,
        )?;
        let executable_root = HostDir::open(
            std::path::Path::new("/bin"),
            RootPolicy::AllComponentsNoFollow,
        )?;
        let file = open_executable(&executable_root)?;
        let executable_fingerprint = fingerprint(&file)?;
        if executable_fingerprint.canonical_bytes() != authority.qualification.launchctl_fingerprint
        {
            return Err(invalid(
                "launchctl differs from supplied executable qualification",
            ));
        }
        let mut instance = [0; 32];
        crate::random_bytes(&mut instance)?;
        Ok(Self {
            authority,
            root,
            executable: ExecutableOwner {
                root: executable_root,
                file,
                fingerprint: executable_fingerprint,
            },
            instance,
            sequence: 0,
            revoked: false,
            qualified: false,
            pending: None,
            _worker: PhantomData,
        })
    }
    pub fn revoke_context(&mut self) {
        self.revoked = true;
    }
    pub fn is_revoked(&self) -> bool {
        self.revoked
    }
    fn verify_context(&mut self) -> io::Result<()> {
        let result = (|| {
            verify_platform(&self.authority)?;
            let root = HostDir::open(
                &self.authority.definition_root,
                RootPolicy::AllComponentsNoFollow,
            )?;
            if !root.same_object(&self.root)? {
                return Err(invalid("original launchd definition root replaced"));
            }
            let bin = HostDir::open(
                std::path::Path::new("/bin"),
                RootPolicy::AllComponentsNoFollow,
            )?;
            if !bin.same_object(&self.executable.root)?
                || fingerprint(&self.executable.file)? != self.executable.fingerprint
                || fingerprint(&open_executable(&bin)?)? != self.executable.fingerprint
            {
                return Err(invalid("original qualified launchctl executable replaced"));
            }
            Ok(())
        })();
        if result.is_err() {
            self.revoked = true;
        }
        result
    }
    fn dispatch(
        &mut self,
        kind: LaunchctlCommand,
        owner: Option<&RegistrationOwner>,
    ) -> io::Result<()> {
        if self.pending.is_some() {
            return Err(invalid(
                "original launchctl command retained; poll without replay",
            ));
        }
        self.verify_context()?;
        let native = self.authority.command(kind)?;
        let definition = read_definition(&self.root, &self.authority.label)?;
        if let Some(owner) = owner
            && !definition.as_ref().is_some_and(|(_, fp, bytes)| {
                fp == &owner.fingerprint && bytes == &owner.definition_bytes
            })
        {
            self.revoked = true;
            return Err(invalid("original plist changed at dispatch boundary"));
        }
        let root = HostDir::open(
            &self.authority.definition_root,
            RootPolicy::AllComponentsNoFollow,
        )?;
        if !root.same_object(&self.root)? {
            self.revoked = true;
            return Err(invalid("plist root replaced before dispatch"));
        }
        let executable = self.executable.file.try_clone()?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| invalid("command sequence exhausted"))?;
        let instance = self
            .instance
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let id = format!("launchctl-command-{instance}-{}", self.sequence);
        // Retain all command/file owners before the uncertain native launch.
        self.pending = Some(PendingCommand {
            kind,
            id: id.clone(),
            command: None,
            definition,
            _root: root,
            _executable: executable,
            progress: OwnedActionProgress::Pending { job: Some(id) },
        });
        let command = spawn_native(&native, self.authority.qualification.clone());
        match command {
            Ok(command) => self.pending.as_mut().expect("prepared command").command = Some(command),
            Err(error) => {
                self.pending = None;
                return Err(error);
            }
        }
        Ok(())
    }
    pub fn inspect(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<ServiceObservation> {
        remaining(deadline, cancel)?;
        self.dispatch(LaunchctlCommand::Inspect, None)?;
        self.poll_inspect(deadline, cancel)
    }
    /// Observe against the original qualified file before and after the retained
    /// read command. Only an accepted SUT uninstall receipt permits file absence;
    /// a same-name replacement is never admitted, even during uninstall.
    pub fn inspect_registration(
        &mut self,
        owner: &mut RegistrationOwner,
        accepted_uninstall: Option<&str>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<ServiceObservation> {
        if let Some(receipt) = accepted_uninstall
            && (receipt.is_empty() || receipt.len() > 4096 || receipt.chars().any(char::is_control))
        {
            return Err(invalid("accepted SUT uninstall receipt required"));
        }
        let allow_missing = accepted_uninstall.is_some();
        self.verify_registration_observation(owner, allow_missing)?;
        if self.pending_inspection().is_none() {
            remaining(deadline, cancel)?;
            self.dispatch(LaunchctlCommand::Inspect, None)?;
        }
        let observed = self.poll_inspect_inner(deadline, cancel, allow_missing)?;
        self.verify_registration_observation(owner, allow_missing)?;
        Ok(observed)
    }
    /// Read-only validation for independently observed readiness. This never
    /// permits removal or manufactures a replacement registration owner.
    pub fn verify_registration(&mut self, owner: &mut RegistrationOwner) -> io::Result<()> {
        self.verify_registration_observation(owner, false)
    }
    fn verify_registration_observation(
        &mut self,
        owner: &mut RegistrationOwner,
        allow_missing: bool,
    ) -> io::Result<()> {
        let result = if self.revoked {
            Err(invalid("original launchd context permanently revoked"))
        } else {
            self.verify_context().and_then(|()| {
                verify_original_observation(
                    &self.authority,
                    &self.root,
                    owner,
                    self.instance,
                    allow_missing,
                )
            })
        };
        if result.is_err() {
            owner.revoked = true;
            self.revoked = true;
        }
        result
    }
    /// Resume only the retained read command after deadline/cancellation.
    pub fn poll_inspect(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<ServiceObservation> {
        self.poll_inspect_inner(deadline, cancel, false)
    }
    fn poll_inspect_inner(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
        allow_sut_removal: bool,
    ) -> io::Result<ServiceObservation> {
        let pending = self
            .pending
            .as_mut()
            .filter(|p| p.kind == LaunchctlCommand::Inspect)
            .ok_or_else(|| invalid("no original print command"))?;
        let command = pending
            .command
            .as_mut()
            .ok_or_else(|| invalid("native command launch unresolved"))?;
        if let Err(error) = command.poll(deadline, cancel) {
            let reason = command.error(error);
            pending.progress = OwnedActionProgress::Unknown {
                job: Some(pending.id.clone()),
                reason: reason.clone(),
            };
            return Err(io::Error::other(reason));
        }
        self.verify_context()?;
        let pending = self.pending.take().expect("original print retained");
        let evidence = pending
            .command
            .as_ref()
            .expect("original print command")
            .evidence();
        let parsed = normalize_print(&self.authority, evidence);
        let current = read_definition(&self.root, &self.authority.label)?;
        let unchanged = match (&pending.definition, &current) {
            (None, None) => true,
            (Some((_, fp, bytes)), Some((_, now, contents))) => fp == now && bytes == contents,
            _ => false,
        };
        if !unchanged {
            self.revoked = true;
            return Err(invalid("plist changed across original print observation"));
        }
        if matches!(parsed.registration, Fact::Unknown(_))
            || (self.qualified && !allow_sut_removal && parsed.registration == Fact::Known(false))
        {
            self.revoked = true;
        }
        let definition_bytes = pending.definition.map(|(_, _, bytes)| bytes);
        Ok(ServiceObservation {
            registration: parsed.registration,
            definition_present: definition_bytes.is_some(),
            definition_bytes,
            loaded: parsed.loaded,
            evidence: parsed.evidence,
        })
    }
    /// Only after the helper verifies the original durably accepted SUT install
    /// receipt and provider's pre-reserved exclusive namespace. This never adopts
    /// a preexisting observed job and never manufactures registration generation.
    pub fn qualify_registration(
        &mut self,
        definition: &LaunchdDefinition,
        install_receipt: String,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<RegistrationOwner> {
        if self.revoked || self.qualified {
            return Err(invalid("original registration qualification unavailable"));
        }
        definition.validate()?;
        if install_receipt.is_empty()
            || install_receipt.len() > 4096
            || install_receipt.chars().any(char::is_control)
        {
            return Err(invalid("original accepted SUT install receipt required"));
        }
        let observation = self.inspect(deadline, cancel)?;
        let result = (|| {
            check_loaded(&self.authority, &observation.loaded, definition)?;
            let Some((file, fp, bytes)) = read_definition(&self.root, &self.authority.label)?
            else {
                return Err(invalid("original installed plist absent"));
            };
            if observation.definition_bytes.as_ref() != Some(&bytes) {
                return Err(invalid("original plist changed after print"));
            }
            validate_plist(&bytes, &self.authority, definition)?;
            Ok(RegistrationOwner {
                file,
                fingerprint: fp,
                definition_bytes: bytes,
                definition: definition.clone(),
                manager_instance: self.instance,
                install_receipt,
                revoked: false,
            })
        })();
        if result.is_err() {
            self.revoked = true;
        } else {
            self.qualified = true;
        }
        result
    }
    pub fn action(
        &mut self,
        owner: &mut RegistrationOwner,
        action: ServiceAction,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<OwnedActionProgress> {
        remaining(deadline, cancel)?;
        if self.pending.is_some() {
            return Err(invalid(
                "original native command retained; no action replay",
            ));
        }
        if self.revoked || owner.is_revoked() {
            owner.revoked = true;
            return Err(invalid("launchd authority permanently revoked"));
        }
        self.authority.command(LaunchctlCommand::Runtime(action))?;
        let result = (|| {
            verify_original(&self.authority, &self.root, owner, self.instance)?;
            let observation = self.inspect(deadline, cancel)?;
            check_loaded(&self.authority, &observation.loaded, &owner.definition)?;
            verify_original(&self.authority, &self.root, owner, self.instance)?;
            self.dispatch(LaunchctlCommand::Runtime(action), Some(owner))?;
            Ok(())
        })();
        if let Err(error) = result {
            self.revoked = true;
            owner.revoked = true;
            return Err(error);
        }
        self.poll_action(deadline, cancel)
    }
    /// Exposes a retained read command after inspect deadline/cancellation so the
    /// helper can keep its disposition unresolved and resume poll_inspect.
    pub fn pending_inspection(&self) -> Option<OwnedActionProgress> {
        self.pending
            .as_ref()
            .filter(|p| p.kind == LaunchctlCommand::Inspect)
            .map(|p| p.progress.clone())
    }
    pub fn pending_action(&self) -> Option<OwnedActionProgress> {
        self.pending
            .as_ref()
            .filter(|p| matches!(p.kind, LaunchctlCommand::Runtime(_)))
            .map(|p| p.progress.clone())
    }
    pub fn poll_action(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<OwnedActionProgress> {
        let pending = self
            .pending
            .as_mut()
            .filter(|p| matches!(p.kind, LaunchctlCommand::Runtime(_)))
            .ok_or_else(|| invalid("no original launchctl action"))?;
        if matches!(pending.progress, OwnedActionProgress::Terminal { .. }) {
            return Ok(pending.progress.clone());
        }
        let command = pending
            .command
            .as_mut()
            .ok_or_else(|| invalid("original native launch unresolved"))?;
        let native_result = command.poll(deadline, cancel);
        let native_completed = native_result.is_ok();
        let verified_result = match native_result {
            Ok(_) => self.verify_context().and_then(|()| {
                let current = read_definition(&self.root, &self.authority.label)?;
                let pending = self.pending.as_ref().expect("original command retained");
                match (&pending.definition, &current) {
                    (Some((file, fp, bytes)), Some((_, current_fp, current_bytes)))
                        if fp == current_fp
                            && bytes == current_bytes
                            && fingerprint(file)? == *fp =>
                    {
                        Ok(())
                    }
                    _ => Err(invalid(
                        "original registration changed during runtime command",
                    )),
                }
            }),
            Err(error) => Err(error),
        };
        let pending = self.pending.as_mut().expect("original command retained");
        let command = pending
            .command
            .as_mut()
            .expect("original native owner retained");
        pending.progress = match verified_result {
            Ok(()) => OwnedActionProgress::Terminal {
                job: pending.id.clone(),
                action: match pending.kind {
                    LaunchctlCommand::Runtime(action) => action,
                    LaunchctlCommand::Inspect => unreachable!("runtime command only"),
                },
                completion: command.completion(),
                result: command.outcome(),
            },
            Err(error) => {
                // Even a command that exits cannot restore lost registration
                // authority. Keep its receipt and original native errors.
                if native_completed {
                    self.revoked = true;
                }
                OwnedActionProgress::Unknown {
                    job: Some(pending.id.clone()),
                    reason: command.error(error),
                }
            }
        };
        Ok(pending.progress.clone())
    }
    pub fn take_terminal_action(&mut self) -> io::Result<OwnedActionProgress> {
        if !matches!(
            self.pending_action(),
            Some(OwnedActionProgress::Terminal { .. })
        ) {
            return Err(invalid("original launchctl command not terminal"));
        }
        Ok(self
            .pending
            .take()
            .expect("terminal original command")
            .progress)
    }
}
impl Drop for LaunchdManager {
    fn drop(&mut self) {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| !matches!(p.progress, OwnedActionProgress::Terminal { .. }))
            && let Some(pending) = self.pending.take()
        {
            std::mem::forget(pending);
        }
    }
}
fn check_loaded(
    authority: &LaunchdAuthority,
    loaded: &Fact<LoadedService>,
    definition: &LaunchdDefinition,
) -> io::Result<()> {
    match loaded {
        Fact::Known(loaded)
            if loaded.label == authority.label
                && loaded.domain == authority.domain
                && Some(loaded.origin.as_str()) == authority.plist_path().to_str()
                && &loaded.definition == definition =>
        {
            Ok(())
        }
        _ => Err(invalid(
            "original loaded domain/origin/configuration unproved",
        )),
    }
}
fn open_executable(root: &HostDir) -> io::Result<File> {
    match root.open_leaf_classified(std::ffi::OsStr::new("launchctl")) {
        crate::fs::LeafOpenResult::Opened(file) => Ok(file),
        crate::fs::LeafOpenResult::MissingSettled => Err(invalid("qualified launchctl missing")),
        crate::fs::LeafOpenResult::FailedSettled(error) => Err(error),
        crate::fs::LeafOpenResult::Unsettled(owner) => Err(io::Error::other(owner)),
    }
}
#[cfg(target_os = "macos")]
fn spawn_native(
    command: &crate::process::NativeCommand,
    qualification: PrintQualification,
) -> io::Result<CommandOwner> {
    Ok(CommandOwner::spawn(command, qualification))
}
#[cfg(not(target_os = "macos"))]
fn spawn_native(
    _command: &crate::process::NativeCommand,
    _qualification: PrintQualification,
) -> io::Result<CommandOwner> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "launchctl execution requires native macOS",
    ))
}
#[cfg(target_os = "macos")]
fn verify_platform(authority: &LaunchdAuthority) -> io::Result<()> {
    let qualification = &authority.qualification;
    let LaunchdDomain::User(uid) = authority.domain else {
        return Err(invalid("only explicit user domain is admitted"));
    };
    if unsafe { libc::geteuid() } != uid {
        return Err(invalid(
            "helper principal differs from supplied user domain",
        ));
    }
    fn sysctl(name: &std::ffi::CStr) -> io::Result<String> {
        let mut bytes = [0u8; 128];
        let mut length = bytes.len();
        let result = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        if length == 0 || length > bytes.len() || bytes[length - 1] != 0 {
            return Err(invalid("unbounded native OS qualification"));
        }
        std::str::from_utf8(&bytes[..length - 1])
            .map(str::to_owned)
            .map_err(|_| invalid("invalid native OS qualification"))
    }
    if sysctl(c"kern.osproductversion")? != qualification.os_version
        || sysctl(c"kern.osversion")? != qualification.os_build
    {
        return Err(invalid("native OS/build outside supplied qualification"));
    }
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn verify_platform(_authority: &LaunchdAuthority) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "launchd requires native macOS qualification",
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launchd_wrong_domain_origin_or_unknown_loaded_facts_cannot_qualify_controls() {
        let authority = super::super::model::tests::authority();
        let definition = LaunchdDefinition {
            executable: "/fixture/service".into(),
            arguments: vec!["/fixture/service".into()],
            working_directory: "/fixture".into(),
        };
        let loaded = LoadedService {
            label: "org.theseus.fixture".into(),
            domain: LaunchdDomain::User(501),
            origin: "/fixture/plists/org.theseus.fixture.plist".into(),
            definition: definition.clone(),
            state: RuntimeState::NotRunning,
            pid: Fact::Unavailable("no live process".into()),
            registration_generation: Fact::Unavailable("not offered".into()),
            readiness: Fact::Unavailable("independent challenge required".into()),
        };
        assert!(check_loaded(&authority, &Fact::Known(loaded.clone()), &definition).is_ok());
        for domain in [
            LaunchdDomain::User(502),
            LaunchdDomain::Gui(501),
            LaunchdDomain::System,
        ] {
            let mut changed = loaded.clone();
            changed.domain = domain;
            assert!(check_loaded(&authority, &Fact::Known(changed), &definition).is_err());
        }
        let mut changed = loaded.clone();
        changed.origin = "/other/org.theseus.fixture.plist".into();
        assert!(check_loaded(&authority, &Fact::Known(changed), &definition).is_err());
        let mut changed = loaded;
        changed.definition.arguments.push("unadmitted".into());
        assert!(check_loaded(&authority, &Fact::Known(changed), &definition).is_err());
        assert!(
            check_loaded(
                &authority,
                &Fact::Unknown("exit0 insufficient".into()),
                &definition
            )
            .is_err()
        );
        assert!(
            check_loaded(
                &authority,
                &Fact::Unavailable("unknown schema".into()),
                &definition
            )
            .is_err()
        );
    }
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn launchd_native_execution_is_refused_before_host_contact_on_linux() {
        assert!(
            matches!(LaunchdManager::connect(super::super::model::tests::authority(),Instant::now()+std::time::Duration::from_secs(1),&AtomicBool::new(false)),Err(e) if e.kind()==io::ErrorKind::Unsupported)
        );
    }
}
