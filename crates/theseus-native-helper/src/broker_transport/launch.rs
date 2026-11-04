use super::{
    BrokerServerConfig,
    connection::{endpoint, principal},
};
use crate::broker::{OwnedLaunch, PreparedActivation};
use sha2::{Digest as _, Sha256};
use std::{cell::Cell, ffi::OsStr, fs::File, io, path::PathBuf};
use theseus_host::{
    desktop::{
        owned::ExecutableBinding,
        session::{
            BoundSession, DesktopKind, DesktopMode, SessionAuthority, SessionBinding,
            SuppliedDisplay,
        },
        transport::{DirectoryOpenResult, open_child_directory_classified},
    },
    fs::{
        FileFingerprint, HostDir, LeafOpenResult, RelativePath, RootPolicy, fingerprint,
        read_exact_at,
    },
    process::{ChildEnvironment, NativeCommand},
    services::readiness as host,
};
use theseus_native_protocol::v2::{
    self,
    broker::{DesktopAction, DesktopRequest},
};
pub(super) struct PinnedFile {
    root: HostDir,
    path: PathBuf,
    relative: RelativePath,
    file: File,
    fingerprint: FileFingerprint,
    length: u64,
    sha256: [u8; 32],
}
impl PinnedFile {
    fn open(root: &str, relative: &str, length: u64, sha256: &str) -> io::Result<Self> {
        let root_path = PathBuf::from(root);
        let directory = HostDir::open(&root_path, RootPolicy::AllComponentsNoFollow)?;
        let relative_path = RelativePath::parse(relative)?;
        let file = directory.open_file(&relative_path)?;
        let fingerprint = fingerprint(&file)?;
        let owner = Self {
            root: directory,
            path: root_path,
            relative: relative_path,
            file,
            fingerprint,
            length,
            sha256: *v2::Digest::parse(sha256)
                .map_err(io::Error::other)?
                .as_bytes(),
        };
        owner.revalidate()?;
        Ok(owner)
    }
    pub fn revalidate(&self) -> io::Result<()> {
        if self.length == 0
            || self.length > v2::MAX_NATIVE_FILE_BYTES
            || self.fingerprint.len != self.length
        {
            return Err(invalid("admitted artifact length mismatch"));
        }
        let root = HostDir::open(&self.path, RootPolicy::AllComponentsNoFollow)?;
        if !self.root.same_object(&root)?
            || fingerprint(&self.file)? != self.fingerprint
            || fingerprint(&self.root.open_file(&self.relative)?)? != self.fingerprint
        {
            return Err(invalid("original desktop artifact/root replaced"));
        }
        let mut hash = Sha256::new();
        let mut buffer = [0; 32768];
        let mut offset = 0;
        while offset < self.length {
            let n = (self.length - offset).min(buffer.len() as u64) as usize;
            read_exact_at(&self.file, &mut buffer[..n], offset)?;
            hash.update(&buffer[..n]);
            offset += n as u64;
        }
        let actual: [u8; 32] = hash.finalize().into();
        if actual != self.sha256 || fingerprint(&self.file)? != self.fingerprint {
            return Err(invalid("original artifact content changed"));
        }
        Ok(())
    }
}
pub(super) struct DefinitionRoots {
    entry: HostDir,
    executable: HostDir,
    entry_path: PathBuf,
    executable_path: PathBuf,
    revoked: Cell<bool>,
}
impl DefinitionRoots {
    pub fn open(config: &BrokerServerConfig) -> io::Result<Self> {
        let d = config
            .manifest
            .desktop
            .as_ref()
            .ok_or_else(|| invalid("desktop absent"))?;
        let x = config
            .manifest
            .desktop_executable
            .as_ref()
            .ok_or_else(|| invalid("executable absent"))?;
        Ok(Self {
            entry: HostDir::open(
                std::path::Path::new(&d.entry_root),
                RootPolicy::AllComponentsNoFollow,
            )?,
            executable: HostDir::open(
                std::path::Path::new(&x.root),
                RootPolicy::AllComponentsNoFollow,
            )?,
            entry_path: d.entry_root.clone().into(),
            executable_path: x.root.clone().into(),
            revoked: Cell::new(false),
        })
    }
    pub fn revalidate(&self) -> io::Result<()> {
        if self.revoked.get() {
            return Err(invalid("original desktop roots permanently revoked"));
        }
        let result = (|| {
            if !self.entry.same_object(&HostDir::open(
                &self.entry_path,
                RootPolicy::AllComponentsNoFollow,
            )?)? || !self.executable.same_object(&HostDir::open(
                &self.executable_path,
                RootPolicy::AllComponentsNoFollow,
            )?)? {
                return Err(invalid("original desktop entry/executable root replaced"));
            }
            Ok(())
        })();
        if result.is_err() {
            self.revoked.set(true);
        }
        result
    }
    fn binding_hash(&self, config: &BrokerServerConfig, lease: v2::LeaseId) -> io::Result<Sha256> {
        self.revalidate()?;
        let mut hash = Sha256::new();
        hash.update(b"theseus.desktop.original-artifact-binding-v1\0");
        hash.update(config.header.digest().map_err(io::Error::other)?.as_bytes());
        hash.update(lease.as_bytes());
        hash.update(theseus_host::desktop::transport::retained_directory_identity(&self.entry)?);
        hash.update(
            theseus_host::desktop::transport::retained_directory_identity(&self.executable)?,
        );
        Ok(hash)
    }
    pub fn vacant_binding(
        &self,
        config: &BrokerServerConfig,
        lease: v2::LeaseId,
    ) -> io::Result<String> {
        let mut hash = self.binding_hash(config, lease)?;
        hash.update(b"vacant\0");
        Ok(format!("{:x}", hash.finalize()))
    }
}
pub(super) struct RetainedDefinition {
    #[cfg(target_os = "linux")]
    linux: Option<theseus_host::desktop::linux::RetainedEntry>,
    #[cfg(target_os = "macos")]
    macos: Option<theseus_host::desktop::macos::AdmittedBundle>,
    #[cfg(windows)]
    windows: Option<theseus_host::desktop::windows::AdmittedShortcut>,
    entry: PinnedFile,
    executable: PinnedFile,
}
impl RetainedDefinition {
    pub fn admit(
        config: &BrokerServerConfig,
        roots: &DefinitionRoots,
        tuple: &v2::DesktopDefinition,
        #[cfg(windows)] apartment: &mut Option<theseus_host::desktop::windows::ComApartment>,
    ) -> io::Result<Self> {
        roots.revalidate()?;
        let _ = config;
        let d = &tuple.desktop;
        let x = &tuple.executable;
        let relative = tuple
            .native
            .as_ref()
            .map_or_else(|| d.entry.clone(), |n| n.entry_relative_path(d));
        let entry = PinnedFile::open(
            &d.entry_root,
            &relative,
            tuple.entry_length,
            &d.entry_digest,
        )?;
        let executable = PinnedFile::open(&x.root, &x.relative_path, x.length, &x.sha256)?;
        #[cfg(target_os = "linux")]
        let linux = Some(theseus_host::desktop::linux::RetainedEntry::admit(
            theseus_host::desktop::linux::AdmittedDefinition {
                entry: theseus_host::desktop::linux::ArtifactSlot {
                    root: d.entry_root.clone().into(),
                    relative: d.entry.clone(),
                    length: tuple.entry_length,
                    sha256: *v2::Digest::parse(&d.entry_digest)
                        .map_err(io::Error::other)?
                        .as_bytes(),
                },
                executable: theseus_host::desktop::linux::ArtifactSlot {
                    root: x.root.clone().into(),
                    relative: x.relative_path.clone(),
                    length: x.length,
                    sha256: *v2::Digest::parse(&x.sha256)
                        .map_err(io::Error::other)?
                        .as_bytes(),
                },
                command: theseus_host::desktop::linux::LiteralCommand {
                    executable: x.arguments[0].clone(),
                    arguments: x.arguments[1..].to_vec(),
                },
            },
        )?);
        #[cfg(target_os = "macos")]
        let macos = if tuple.native.is_some() {
            Some(super::native_macos::admit(tuple)?)
        } else {
            None
        };
        #[cfg(windows)]
        let windows = if tuple.native.is_some() {
            Some(super::native_windows::admit(
                tuple,
                apartment
                    .take()
                    .ok_or_else(|| invalid("original COM apartment absent"))?,
            )?)
        } else {
            None
        };
        #[cfg(not(target_os = "linux"))]
        if tuple.native.is_none() && !source_headless(config) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native entry adapter qualification required for interactive non-Linux owned launch",
            ));
        }
        let owner = Self {
            #[cfg(target_os = "linux")]
            linux,
            #[cfg(target_os = "macos")]
            macos,
            #[cfg(windows)]
            windows,
            entry,
            executable,
        };
        owner.revalidate()?;
        roots.revalidate()?;
        Ok(owner)
    }
    pub fn revalidate(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(linux) = &self.linux {
            linux.revalidate()?;
        }
        #[cfg(target_os = "macos")]
        if let Some(macos) = &self.macos {
            macos.revalidate()?;
        }
        #[cfg(windows)]
        if let Some(windows) = &self.windows {
            windows.revalidate()?;
        }
        self.entry.revalidate()?;
        self.executable.revalidate()
    }
    #[cfg(windows)]
    pub fn pump(&self) -> io::Result<()> {
        if let Some(owner) = &self.windows {
            owner.pump_once()?;
        }
        Ok(())
    }
    pub fn binding_digest(
        &self,
        config: &BrokerServerConfig,
        roots: &DefinitionRoots,
        lease: v2::LeaseId,
    ) -> io::Result<String> {
        let mut hash = roots.binding_hash(config, lease)?;
        hash.update(b"present\0");
        hash.update(self.entry.fingerprint.canonical_bytes());
        hash.update(self.executable.fingerprint.canonical_bytes());
        hash.update(self.entry.sha256);
        hash.update(self.executable.sha256);
        Ok(format!("{:x}", hash.finalize()))
    }
}
#[cfg(not(target_os = "linux"))]
pub(super) fn source_headless(config: &BrokerServerConfig) -> bool {
    #[cfg(feature = "test-support")]
    if matches!(
        config.header.display,
        v2::broker::SuppliedDisplay::SourceOwnedHeadless
    ) {
        return true;
    }
    let _ = config;
    false
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn check_native_startup_cwd(
    expected: &std::path::Path,
    actual: &std::path::Path,
) -> io::Result<()> {
    if expected != actual {
        return Err(invalid(
            "native broker startup cwd differs from committed ceiling",
        ));
    }
    Ok(())
}
pub(super) fn binding(
    config: &BrokerServerConfig,
    measured: &theseus_host::desktop::transport::MeasuredIdentity,
) -> io::Result<BoundSession> {
    let h = &config.header;
    if config.manifest.desktop_native.is_some() {
        // Native environment commits this broker's original replacement startup
        // environment only; it makes no claim about a manager-launched app env.
        let x = config
            .manifest
            .desktop_executable
            .as_ref()
            .ok_or_else(|| invalid("native executable absent"))?;
        let expected: std::collections::BTreeMap<_, _> = x
            .environment
            .iter()
            .map(|e| {
                (
                    std::ffi::OsString::from(&e.name),
                    std::ffi::OsString::from(&e.value),
                )
            })
            .collect();
        let actual: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
        if actual != expected {
            return Err(invalid(
                "native broker startup environment differs from committed ceiling",
            ));
        }
        // Native Linux's closed .desktop subset forbids Path=. Native Mac
        // LaunchServices has no promised app cwd. In these two modes this field
        // commits only the broker's original startup cwd; never an app-cwd setter.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        check_native_startup_cwd(
            std::path::Path::new(&x.working_directory),
            &std::env::current_dir()?,
        )?;
    }
    #[allow(unreachable_patterns)]
    let display = match &h.display {
        v2::broker::SuppliedDisplay::X11 {
            display,
            authority_file,
            bus,
        } => SuppliedDisplay::X11 {
            display: display.clone(),
            authority_file: authority_file.clone(),
            bus: host::UnixPath::new(bus.clone())?,
        },
        v2::broker::SuppliedDisplay::Wayland {
            socket,
            runtime_directory,
            bus,
        } => SuppliedDisplay::Wayland {
            socket: host::UnixPath::new(socket.clone())?,
            runtime_directory: runtime_directory.clone(),
            bus: host::UnixPath::new(bus.clone())?,
        },
        v2::broker::SuppliedDisplay::MacOsGui { domain } => SuppliedDisplay::MacOsGui {
            domain: domain.clone(),
        },
        v2::broker::SuppliedDisplay::WindowsInteractive {
            session_id,
            window_station,
            desktop,
        } => {
            if measured.session_id != Some(*session_id)
                || measured.window_station.as_ref() != Some(window_station)
                || measured.desktop.as_ref() != Some(desktop)
            {
                return Err(invalid(
                    "measured native interactive context differs from supplied broker session",
                ));
            }
            SuppliedDisplay::WindowsInteractive {
                session_id: *session_id,
                window_station: window_station.clone(),
                desktop: desktop.clone(),
            }
        }
        #[cfg(feature = "test-support")]
        v2::broker::SuppliedDisplay::SourceOwnedHeadless => SuppliedDisplay::SourceOwnedHeadless,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "source headless requires broker test-support build",
            ));
        }
    };
    let expected = SessionBinding {
        target_id: h.target_id.clone(),
        ownership_id: *v2::BindingId::parse(&h.ownership_id)
            .map_err(io::Error::other)?
            .as_bytes(),
        provider_epoch: digest(&h.provider_epoch)?,
        session_id: h.session_id.clone(),
        session_epoch: digest(&h.session_epoch)?,
        broker_header_digest: *h.digest().map_err(io::Error::other)?.as_bytes(),
        manifest_digest: digest(&h.manifest_digest)?,
        broker_endpoint: endpoint(&h.broker_endpoint)?,
        broker_principal: principal(&h.broker_principal),
        helper_principal: principal(&h.helper_principal),
        display,
    };
    let mut observed = expected.clone();
    observed.broker_principal = measured.principal.clone();
    let kind = match h.os.as_str() {
        "linux" if cfg!(target_os = "linux") => DesktopKind::Linux,
        "macos" if cfg!(target_os = "macos") => DesktopKind::MacOs,
        "windows" if cfg!(windows) => DesktopKind::Windows,
        _ => return Err(invalid("broker OS differs from native process")),
    };
    let bound = BoundSession::admit(
        SessionAuthority {
            kind,
            binding: expected,
            allowed_modes: h
                .allowed_modes
                .iter()
                .map(|mode| match mode {
                    v2::broker::DesktopMode::OwnedExecutable => DesktopMode::OwnedExecutable,
                    v2::broker::DesktopMode::NativeActivation => DesktopMode::NativeActivation,
                })
                .collect(),
        },
        &observed,
    )
    .map_err(io::Error::from)?;
    #[cfg(target_os = "macos")]
    if config.manifest.desktop_native.is_some() {
        theseus_host::desktop::macos::validate_supplied_session(
            &bound,
            h.macos_security_session_id
                .ok_or_else(|| invalid("native Mac security session absent"))?,
        )?;
    }
    Ok(bound)
}
// Keep the independently admitted tuple, original owner and channel/scope rights explicit.
#[allow(clippy::too_many_arguments)]
pub(super) fn prepare(
    config: &BrokerServerConfig,
    request: &DesktopRequest,
    session: BoundSession,
    ready_channel: &mut Option<host::LocalChallenge>,
    close_channel: &mut Option<host::LocalChallenge>,
    #[cfg(target_os = "linux")] scope: &mut Option<
        theseus_host::desktop::scope::DedicatedBrokerScope,
    >,
    tuple: &v2::DesktopDefinition,
    #[cfg(target_os = "linux")] native_factory: &mut Option<
        super::native_linux_factory::LinuxNativeFactory,
    >,
    definition: &mut RetainedDefinition,
) -> io::Result<PreparedActivation> {
    let d = &tuple.desktop;
    let x = &tuple.executable;
    let expected = &tuple.readiness;
    let DesktopAction::Activate {
        mode,
        entry_digest,
        expectation_digest,
        close_binding,
        ..
    } = &request.action
    else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "only original declared owned activation supported",
        ));
    };
    if !d.allowed_modes.contains(mode)
        || request.slot != d.slot
        || entry_digest != &d.entry_digest
        || expectation_digest != &d.expectation_digest
        || request.binding_digest != config.header.digest().map_err(io::Error::other)?.to_hex()
    {
        return Err(invalid(
            "activation differs from admitted desktop definition",
        ));
    }
    let action = request.digest().map_err(io::Error::other)?;
    let ready = host::ReadinessChallenge {
        protocol: host::ReadyProtocol::TheseusFixtureReadyV1,
        lease: request.broker_lease.clone(),
        slot: request.slot.as_str().into(),
        resource_binding_digest: digest(&request.prior_binding_digest)?,
        start_or_activation_operation: request.helper_operation.clone(),
        action_digest: *action.as_bytes(),
        nonce: *v2::BindingId::parse(&request.activation_nonce)
            .map_err(io::Error::other)?
            .as_bytes(),
        expectation_digest: digest(expectation_digest)?,
    };
    let mut close = ready.clone();
    close.start_or_activation_operation = close_binding.operation.clone();
    close.action_digest = digest(&close_binding.action_digest)?;
    close.nonce = *v2::BindingId::parse(&close_binding.nonce)
        .map_err(io::Error::other)?
        .as_bytes();
    let binding = ExecutableBinding {
        ready,
        close,
        close_original_activation_operation: request.helper_operation.clone(),
        expectation: host::ReadinessExpectation {
            artifact: if expected.artifact == "base" {
                host::ArtifactRole::Base
            } else {
                host::ArtifactRole::Target
            },
            artifact_length: expected.artifact_length,
            artifact_sha256: digest(&expected.artifact_sha256)?,
            version_literal: expected.version_literal.clone(),
            data_sha256: digest(&expected.data_sha256)?,
            configuration_digest: digest(&expected.configuration_digest)?,
        },
        cooperative_instance: action.to_hex(),
        instance_binding_digest: *action.as_bytes(),
    };
    binding.validate()?;
    if *mode == v2::broker::DesktopMode::NativeActivation {
        if ready_channel.is_none() || close_channel.is_none() {
            return Err(invalid("original native channels consumed"));
        }
        #[cfg(target_os = "linux")]
        {
            let factory = native_factory.as_mut().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "original authenticated Linux factory unavailable",
                )
            })?;
            let entry = definition
                .linux
                .take()
                .ok_or_else(|| invalid("original admitted Linux entry consumed"))?;
            return factory
                .prepare(
                    config,
                    tuple,
                    session,
                    binding,
                    entry,
                    ready_channel.take().expect("checked"),
                    close_channel.take().expect("checked"),
                )
                .map(|owner| PreparedActivation::LinuxGio(Box::new(owner)));
        }
        #[cfg(target_os = "macos")]
        {
            let entry = definition
                .macos
                .take()
                .ok_or_else(|| invalid("original admitted bundle consumed"))?;
            return super::native_macos::prepare(
                config,
                tuple,
                session,
                binding,
                entry,
                ready_channel.take().expect("checked"),
                close_channel.take().expect("checked"),
            )
            .map(|owner| PreparedActivation::Macos(Box::new(owner)));
        }
        #[cfg(windows)]
        {
            let entry = definition
                .windows
                .take()
                .ok_or_else(|| invalid("original admitted shortcut consumed"))?;
            return super::native_windows::prepare(
                config,
                tuple,
                session,
                binding,
                entry,
                ready_channel.take().expect("checked"),
                close_channel.take().expect("checked"),
            )
            .map(|owner| PreparedActivation::Windows(Box::new(owner)));
        }
    }
    // Installed-image semantics retain ordinary native pathname launch. The
    // validated full manifest requires exclusive_quiescent namespace policy:
    // the operator/SUT must exclude mapping/content/ancestor changes through
    // effects. Retained descriptors/checks detect observed violations; they do
    // not seal bytes or prevent a hostile same-principal writer racing spawn.
    let mut command = NativeCommand::new(&x.arguments[0]);
    command.args(&x.arguments[1..]);
    command.cwd = Some(x.working_directory.clone().into());
    command.environment = ChildEnvironment::Replace(
        x.environment
            .iter()
            .map(|entry| (entry.name.clone().into(), entry.value.clone().into()))
            .collect(),
    );
    if ready_channel.is_none() || close_channel.is_none() {
        return Err(invalid(
            "original readiness/close channels already consumed",
        ));
    }
    #[cfg(target_os = "linux")]
    if scope.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "dedicated broker permits one original activation",
        ));
    }
    Ok(PreparedActivation::Owned(Box::new(OwnedLaunch {
        command,
        session,
        binding,
        ready_channel: ready_channel
            .take()
            .expect("validated original ready channel"),
        close_channel: close_channel
            .take()
            .expect("validated original close channel"),
        #[cfg(target_os = "linux")]
        scope: scope.take().expect("validated original scope"),
    })))
}
pub(super) fn bind_channel(
    config: &BrokerServerConfig,
    value: &v2::LocalEndpoint,
) -> io::Result<host::LocalChallenge> {
    #[cfg(unix)]
    {
        let _ = config;
        host::LocalChallenge::bind(unix_endpoint(value)?)
    }
    #[cfg(windows)]
    {
        host::LocalChallenge::bind(pipe_endpoint(value)?, broker_sid(config)?)
    }
}
#[cfg(unix)]
fn unix_endpoint(value: &v2::LocalEndpoint) -> io::Result<host::UnixPath> {
    match value {
        v2::LocalEndpoint::UnixPath { path } => host::UnixPath::new(path.clone()),
        _ => Err(invalid("expected explicit Unix readiness endpoint")),
    }
}
#[cfg(windows)]
fn pipe_endpoint(value: &v2::LocalEndpoint) -> io::Result<host::NamedPipe> {
    match value {
        v2::LocalEndpoint::NamedPipe { path } => host::NamedPipe::new(path.clone()),
        _ => Err(invalid("expected explicit named pipe readiness endpoint")),
    }
}
#[cfg(windows)]
fn broker_sid(config: &BrokerServerConfig) -> io::Result<host::Sid> {
    let v2::broker::BrokerPrincipal::Windows { token_user_sid } = &config.header.broker_principal
    else {
        return Err(invalid("Windows broker SID absent"));
    };
    host::Sid::new(token_user_sid.clone())
}
pub(super) struct ObservedFiles {
    pub entry_present: bool,
    pub entry_digest: Option<String>,
    pub executable_digest: Option<String>,
}
fn observed_file(root: &HostDir, relative: &str, max: u64) -> io::Result<Option<String>> {
    RelativePath::parse(relative)?;
    let (leaf, parents) = match relative.rsplit_once('/') {
        Some((parents, leaf)) => (leaf, Some(parents)),
        None => (relative, None),
    };
    let mut chain = Vec::new();
    if let Some(parents) = parents {
        for name in parents.split('/') {
            let parent = chain.last().map(|(_, d)| d).unwrap_or(root);
            match open_child_directory_classified(parent, OsStr::new(name)) {
                DirectoryOpenResult::Opened(directory) => chain.push((name, directory)),
                DirectoryOpenResult::MissingSettled => {
                    validate_observed_parents(root, &chain)?;
                    return Ok(None);
                }
                DirectoryOpenResult::Unproved(error) => return Err(error),
            }
        }
    }
    let parent = chain.last().map(|(_, d)| d).unwrap_or(root);
    let file = match parent.open_leaf_classified(OsStr::new(leaf)) {
        LeafOpenResult::MissingSettled => {
            validate_observed_parents(root, &chain)?;
            return Ok(None);
        }
        LeafOpenResult::Opened(file) => file,
        LeafOpenResult::FailedSettled(error) => return Err(error),
        LeafOpenResult::Unsettled(owner) => return Err(io::Error::other(owner)),
    };
    let before = fingerprint(&file)?;
    if before.len > max {
        return Err(invalid(
            "observed desktop artifact exceeds bounded inspection",
        ));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 32768];
    let mut offset = 0;
    while offset < before.len {
        let n = (before.len - offset).min(buffer.len() as u64) as usize;
        read_exact_at(&file, &mut buffer[..n], offset)?;
        hash.update(&buffer[..n]);
        offset += n as u64;
    }
    if fingerprint(&file)? != before {
        return Err(invalid("observed desktop file changed during read"));
    }
    let mapped = match parent.open_leaf_classified(OsStr::new(leaf)) {
        LeafOpenResult::Opened(file) => file,
        LeafOpenResult::MissingSettled => {
            return Err(invalid("observed desktop file mapping disappeared"));
        }
        LeafOpenResult::FailedSettled(error) => return Err(error),
        LeafOpenResult::Unsettled(owner) => return Err(io::Error::other(owner)),
    };
    if fingerprint(&mapped)? != before {
        return Err(invalid("observed desktop file mapping replaced"));
    }
    validate_observed_parents(root, &chain)?;
    Ok(Some(format!("{:x}", hash.finalize())))
}
fn validate_observed_parents(root: &HostDir, chain: &[(&str, HostDir)]) -> io::Result<()> {
    let mut parent = root;
    for (name, held) in chain {
        let current = parent.open_child_name(OsStr::new(name))?;
        if !held.same_object(&current)? {
            return Err(invalid("observed artifact ancestor mapping replaced"));
        }
        parent = held;
    }
    Ok(())
}
pub(super) fn inspect(
    config: &BrokerServerConfig,
    roots: &DefinitionRoots,
    tuple: &v2::DesktopDefinition,
) -> io::Result<ObservedFiles> {
    roots.revalidate()?;
    let _ = config;
    let d = &tuple.desktop;
    let x = &tuple.executable;
    let relative = tuple
        .native
        .as_ref()
        .map_or_else(|| d.entry.clone(), |n| n.entry_relative_path(d));
    let entry_digest = observed_file(&roots.entry, &relative, 65536)?;
    let entry_present = if matches!(
        tuple.native,
        Some(v2::NativeDesktopDefinition::MacosBundle { .. })
    ) {
        // An app directory with absent metadata remains present/leaked, not absent.
        match open_child_directory_classified(&roots.entry, OsStr::new(&d.entry)) {
            DirectoryOpenResult::Opened(app) => {
                let current = roots.entry.open_child_name(OsStr::new(&d.entry))?;
                if !app.same_object(&current)? {
                    return Err(invalid("original observed app directory replaced"));
                }
                true
            }
            DirectoryOpenResult::MissingSettled => false,
            DirectoryOpenResult::Unproved(error) => return Err(error),
        }
    } else {
        entry_digest.is_some()
    };
    let executable_digest = observed_file(
        &roots.executable,
        &x.relative_path,
        v2::MAX_NATIVE_FILE_BYTES,
    )?;
    roots.revalidate()?;
    Ok(ObservedFiles {
        entry_present,
        entry_digest,
        executable_digest,
    })
}
pub(super) fn digest(value: &str) -> io::Result<[u8; 32]> {
    Ok(*v2::Digest::parse(value)
        .map_err(io::Error::other)?
        .as_bytes())
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
