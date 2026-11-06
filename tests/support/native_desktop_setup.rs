//! Test-owned, desktop-only SourceOwnedExecutable context; no display or manager.
#![allow(dead_code)]
#[cfg(target_os = "linux")]
use crate::combined::service_setup::example_inputs;
#[cfg(not(target_os = "linux"))]
#[path = "native_example_inputs.rs"]
mod example_inputs;
#[path = "native_desktop_installer.rs"]
pub(crate) mod installer;
#[path = "native_desktop_provider.rs"]
mod provider;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use theseus::{
    model::{CheckOutcome, ScenarioKind, StepName},
    native::integration::{model::*, recipe::ValidatedIntegrationRecipe},
};
use theseus_host::fs::HostDir;
use theseus_native_protocol::v2;
#[derive(Clone, Copy)]
pub enum Fault {
    None,
    MissingEntry,
    LeakedEntry,
}
impl Fault {
    fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::MissingEntry => "missing_entry",
            Self::LeakedEntry => "leaked_entry",
        }
    }
}
pub(crate) fn pin(path: &Path) -> Result<Value> {
    let bytes = std::fs::read(path)?;
    Ok(json!({"path":path,"length":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))}))
}
fn artifact(path: &Path) -> Result<Value> {
    let p = pin(path)?;
    Ok(json!({"length":p["length"],"sha256":p["sha256"]}))
}
pub(crate) fn write(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
fn original_child_join(child: &mut Option<Child>, input: &mut Option<ChildStdin>) -> Result<()> {
    input.take();
    let until = Instant::now() + Duration::from_secs(45);
    if let Some(original) = child.as_mut() {
        loop {
            if let Some(status) = original.try_wait()? {
                child.take();
                ensure!(status.success(), "original source broker exit: {status}");
                break;
            }
            if Instant::now() >= until {
                original.kill()?;
                let status = original.wait()?;
                child.take();
                anyhow::bail!(
                    "original source broker failed graceful settlement; forced original-child join: {status}"
                );
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(())
}
#[derive(Default)]
struct BrokerOwner {
    child: Option<Child>,
    input: Option<ChildStdin>,
}
impl Drop for BrokerOwner {
    fn drop(&mut self) {
        if self.child.is_some()
            && let Err(error) = original_child_join(&mut self.child, &mut self.input)
        {
            eprintln!("original broker cleanup remains failed: {error:#}");
        }
    }
}
fn handshake(header: &v2::broker::IntegrationBrokerHeader) -> Result<()> {
    let until = Instant::now() + Duration::from_secs(10);
    let mut connection = loop {
        match theseus_native_helper::broker_transport::BrokerConnection::connect(header) {
            Ok(connection) => break connection,
            Err(failure) => {
                let (error, owner) = failure.into_parts();
                if let Some(mut owner) = owner {
                    owner.request_stop();
                    loop {
                        match owner.poll_settle() {
                            Ok(true) => break,
                            Ok(false) | Err(_) if Instant::now() < until => {
                                thread::sleep(Duration::from_millis(2))
                            }
                            _ => {
                                let _ = Box::leak(owner);
                                anyhow::bail!(
                                    "original startup transport remains retained: {error}"
                                );
                            }
                        }
                    }
                    return Err(error.into());
                }
                ensure!(
                    Instant::now() < until
                        && matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound
                                | std::io::ErrorKind::ConnectionRefused
                                | std::io::ErrorKind::WouldBlock
                        ),
                    "original source broker startup: {error}"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }
    };
    let outcome = (|| -> Result<()> {
        while !connection.poll_ready()? {
            ensure!(Instant::now() < until, "original broker handshake deadline");
            thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    })();
    connection.request_stop();
    while !connection.poll_settle()? {
        ensure!(
            Instant::now() < until,
            "original startup conversation settlement"
        );
        thread::sleep(Duration::from_millis(2));
    }
    outcome
}
pub(crate) struct Operator {
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<Result<()>>>,
}
impl Operator {
    pub(crate) fn start(root: PathBuf, broker: PathBuf, broker_config: PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let worker = thread::spawn(move || -> Result<()> {
            let mut owner = BrokerOwner::default();
            let mut ownership = None;
            let mut handled = std::collections::BTreeSet::new();
            let mut failure = None;
            while !signal.load(Ordering::Acquire) {
                for entry in std::fs::read_dir(&root)? {
                    let path = entry?.path();
                    if path.extension().and_then(|s| s.to_str()) != Some("operator-request")
                        || handled.contains(&path)
                    {
                        continue;
                    }
                    let bytes = std::fs::read(&path)?;
                    ensure!(bytes.len() <= 16384, "bounded operator request");
                    let request: provider::OperatorRequest = serde_json::from_slice(&bytes)?;
                    ensure!(
                        !request.invocation_id.is_empty()
                            && request
                                .invocation_id
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
                        "original operator invocation ID"
                    );
                    let result = (|| -> Result<()> {
                        match request.action.as_str() {
                            "start" => {
                                ensure!(
                                    owner.child.is_none() && ownership.is_none(),
                                    "no broker duplicate start"
                                );
                                let config: Value =
                                    serde_json::from_slice(&std::fs::read(&broker_config)?)?;
                                let header: v2::broker::IntegrationBrokerHeader =
                                    serde_json::from_value(config["header"].clone())?;
                                header.validate()?;
                                ensure!(
                                    header.ownership_id == request.ownership_id
                                        && matches!(
                                            header.display,
                                            v2::broker::SuppliedDisplay::SourceOwnedHeadless
                                        )
                                        && config["source_parent_eof"] == true,
                                    "exact source original broker context"
                                );
                                if let v2::LocalEndpoint::UnixPath { path } =
                                    &header.broker_endpoint
                                {
                                    ensure!(
                                        !Path::new(path).try_exists()?,
                                        "no existing broker socket adoption"
                                    );
                                }
                                let mut original = Command::new(&broker)
                                    .args(["serve", "--config"])
                                    .arg(&broker_config)
                                    .current_dir(root.parent().context("fixture root")?)
                                    .env_clear()
                                    .stdin(Stdio::piped())
                                    // Parent watch uses the existing strict
                                    // inherited-pipe contract. The original
                                    // Child retains stdout through its join.
                                    .stdout(Stdio::piped())
                                    .stderr(std::fs::File::create_new(
                                        root.join("broker-stderr.log"),
                                    )?)
                                    .spawn()?;
                                owner.input = original.stdin.take();
                                owner.child = Some(original);
                                ownership = Some(request.ownership_id.clone());
                                let until = Instant::now() + Duration::from_secs(10);
                                if let v2::LocalEndpoint::UnixPath { path } =
                                    &header.broker_endpoint
                                {
                                    while !Path::new(path).try_exists()? {
                                        ensure!(
                                            owner.child.as_mut().unwrap().try_wait()?.is_none(),
                                            "original broker exited before endpoint"
                                        );
                                        ensure!(
                                            Instant::now() < until,
                                            "original broker startup pending"
                                        );
                                        thread::sleep(Duration::from_millis(5));
                                    }
                                }
                                handshake(&header)?;
                            }
                            "stop" => {
                                ensure!(
                                    ownership.as_ref() == Some(&request.ownership_id),
                                    "stop original broker only"
                                );
                                original_child_join(&mut owner.child, &mut owner.input)?;
                            }
                            _ => anyhow::bail!("unknown source operator action"),
                        }
                        Ok(())
                    })();
                    let detail = result
                        .as_ref()
                        .map(|_| "original broker action settled".to_owned())
                        .unwrap_or_else(|e| format!("{e:#}"));
                    provider::publish(
                        &root.join(format!("{}.operator-reply", request.invocation_id)),
                        &provider::OperatorReply {
                            ownership_id: request.ownership_id,
                            invocation_id: request.invocation_id,
                            action: request.action,
                            success: result.is_ok(),
                            detail,
                        },
                    )?;
                    if let Err(error) = result {
                        failure = Some(error);
                    }
                    handled.insert(path);
                }
                thread::sleep(Duration::from_millis(5));
            }
            let joined = original_child_join(&mut owner.child, &mut owner.input);
            if let Some(error) = failure {
                return Err(error);
            }
            joined
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
    pub(crate) fn finish(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("original broker operator panic"))??;
        }
        Ok(())
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum DesktopEntryKind {
    SourceDocument,
    NativeOwnedEntry,
}
pub struct DesktopFixture {
    directory: Option<tempfile::TempDir>,
    root: PathBuf,
    sockets: Option<tempfile::TempDir>,
    operator: Option<Operator>,
    finished: bool,
    pub controller: PathBuf,
    pub graph: PathBuf,
    pub launch: PathBuf,
    pub recipe: ValidatedIntegrationRecipe,
    pub output: PathBuf,
    pub fault: Fault,
}
impl DesktopFixture {
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn new(fault: Fault) -> Result<Self> {
        Self::new_selected(fault, None, DesktopEntryKind::SourceDocument)
    }
    pub fn new_ordinary(fault: Fault, scenario: ScenarioKind) -> Result<Self> {
        Self::new_selected(fault, Some(scenario), DesktopEntryKind::SourceDocument)
    }
    #[cfg(any(target_os = "macos", windows))]
    pub fn new_owned_entry_ordinary(scenario: ScenarioKind) -> Result<Self> {
        Self::new_selected(
            Fault::None,
            Some(scenario),
            DesktopEntryKind::NativeOwnedEntry,
        )
    }
    fn new_selected(
        fault: Fault,
        scenario: Option<ScenarioKind>,
        entry_kind: DesktopEntryKind,
    ) -> Result<Self> {
        ensure!(
            entry_kind == DesktopEntryKind::SourceDocument
                || cfg!(any(target_os = "macos", windows)),
            "native owned entry fixture requires native platform"
        );
        let temporary = std::env::temp_dir();
        #[cfg(unix)]
        let temporary = temporary.canonicalize()?;
        let directory = tempfile::tempdir_in(temporary)?;
        let fixture_root = directory.path().join("fixture");
        HostDir::create_private(&fixture_root)?;
        let root = fixture_root.as_path();
        #[cfg(unix)]
        let sockets = Some(
            tempfile::Builder::new()
                .prefix("th-desktop-")
                .tempdir_in(Path::new("/tmp").canonicalize()?)?,
        );
        #[cfg(windows)]
        let sockets: Option<tempfile::TempDir> = None;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                sockets.as_ref().unwrap().path(),
                std::fs::Permissions::from_mode(0o700),
            )?;
        }
        for leaf in [
            "workspace",
            "ledger",
            "provider",
            "artifacts",
            "entries",
            "broker-ledger",
            "broker-workspace",
        ] {
            HostDir::create_private(&root.join(leaf))?;
        }
        let artifacts = root.join("artifacts");
        let copy = |variable: &str, name: &str| -> Result<PathBuf> {
            let source = PathBuf::from(
                std::env::var_os(variable)
                    .with_context(|| format!("explicit {variable} required"))?,
            );
            ensure!(
                source.is_absolute() && source.is_file(),
                "absolute source binary pin"
            );
            let destination = artifacts.join(if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.into()
            });
            std::fs::copy(source, &destination)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o500))?;
            }
            Ok(destination)
        };
        let helper = copy("THESEUS_NATIVE_DESKTOP_HELPER_BINARY", "helper")?;
        let broker = copy("THESEUS_NATIVE_DESKTOP_BROKER_BINARY", "broker")?;
        let sut = copy("THESEUS_NATIVE_SERVICE_FIXTURE", "installer")?;
        let base = copy("THESEUS_NATIVE_SERVICE_BASE", "base")?;
        let target = copy("THESEUS_NATIVE_SERVICE_TARGET", "target")?;
        let controller = copy("THESEUS_NATIVE_INTEGRATION_CONTROLLER", "controller")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&artifacts, std::fs::Permissions::from_mode(0o500))?;
        }
        let workspace = root.join("workspace");
        let reader_leaf = if cfg!(windows) {
            "reader.exe"
        } else {
            "reader"
        };
        let reader = workspace.join(reader_leaf);
        let data = workspace.join("data");
        let config_path = root.join("config.json");
        let broker_path = root.join("broker.json");
        let recipe_path = root.join("recipe.json");
        let mut random = [0; 32];
        theseus_host::random_bytes(&mut random)?;
        let epoch = v2::Digest::from_bytes(random);
        let binding = "01".repeat(16);
        let marker = "02".repeat(16);
        let mut programs = vec![];
        for id in [
            "check_base",
            "check_data",
            "check_target",
            "configure",
            "exercise",
            "install_base",
            "install_target",
            "uninstall",
        ] {
            let check = matches!(
                id,
                "check_base" | "check_data" | "check_target" | "exercise"
            );
            programs.push(json!({"id":id,"program":if check{&reader}else{&sut},"args":if check{vec![id.to_owned(),data.display().to_string()]}else{vec![id.to_owned(),config_path.display().to_string(),base.display().to_string(),target.display().to_string(),fault.name().into()]},"environment":{"kind":"replace","entries":[]}}));
        }
        let mut config = json!({"schema":2,"profile":"native_catalog_v1","capabilities":v2::NativeCapabilities::NAMES,"provider_epoch":epoch.to_hex(),"artifacts":{"base":artifact(&base)?,"target":artifact(&target)?},"ledger_root":root.join("ledger"),"workspace_root":workspace,"binding":binding,"manifest_digest":"00".repeat(32),"workspace_marker":marker,"os":installer::os_name(installer::current_os()),"programs":programs});
        let core = theseus_native_helper::config_manifest_digest(&serde_json::to_vec(&config)?)?;
        let expectation = v2::ReadinessDefinitionExpectation {
            role: "configured".into(),
            artifact: "target".into(),
            artifact_length: config["artifacts"]["target"]["length"].as_u64().unwrap(),
            artifact_sha256: config["artifacts"]["target"]["sha256"]
                .as_str()
                .unwrap()
                .into(),
            version_literal: "target\n".into(),
            data_sha256: format!("{:x}", Sha256::digest(b"theseus fixture data\n")),
            configuration_digest: format!("{:x}", Sha256::digest(b"theseus fixture data\n")),
        };
        let channel = |name: &str| -> v2::LocalEndpoint {
            if cfg!(windows) {
                v2::LocalEndpoint::NamedPipe {
                    path: format!(r"\\.\pipe\theseus.{}-{name}", &epoch.to_hex()[..24]),
                }
            } else {
                v2::LocalEndpoint::UnixPath {
                    path: sockets
                        .as_ref()
                        .unwrap()
                        .path()
                        .join(format!("{name}.sock"))
                        .display()
                        .to_string(),
                }
            }
        };
        let ready = channel("ready");
        let close = channel("close");
        let endpoint = channel("broker");
        let endpoint_text = |endpoint: &v2::LocalEndpoint| match endpoint {
            v2::LocalEndpoint::UnixPath { path } | v2::LocalEndpoint::NamedPipe { path } => {
                path.clone()
            }
        };
        let native_entry = entry_kind == DesktopEntryKind::NativeOwnedEntry;
        let executable_root = if native_entry {
            root.join("entries")
        } else {
            workspace.clone()
        };
        let executable_leaf = if native_entry && cfg!(target_os = "macos") {
            "TheseusOwned.app/Contents/MacOS/reader"
        } else {
            reader_leaf
        };
        let executable_path = executable_root.join(executable_leaf);
        let executable = v2::DesktopExecutable {
            root: executable_root.display().to_string(),
            relative_path: executable_leaf.into(),
            length: expectation.artifact_length,
            sha256: expectation.artifact_sha256.clone(),
            arguments: vec![
                executable_path.display().to_string(),
                "desktop".into(),
                endpoint_text(&ready),
                endpoint_text(&close),
                data.display().to_string(),
                "original-source-desktop".into(),
            ],
            environment: vec![],
            working_directory: if native_entry && cfg!(windows) {
                executable_root.display().to_string()
            } else {
                workspace.display().to_string()
            },
        };
        let mut manifest: v2::IntegrationManifest = serde_json::from_value(
            json!({"schema":1,"core_manifest_digest":core.to_hex(),"service_authority":null,"definitions":[],"readiness":null,"desktop_capabilities":["inspect_desktop","owned_executable","request_close"],"artifact_slots":[],"readiness_protocol":"theseus_fixture_ready_v1","desktop":null,"desktop_executable":executable,"desktop_readiness":expectation}),
        )?;
        if cfg!(target_os = "macos") {
            manifest.desktop_owned_lifetime =
                Some(v2::OwnedDesktopLifetimeDefinition::MacosOriginalPrimaryV1);
        }
        if native_entry {
            manifest.desktop_owned_entry = Some(if cfg!(target_os = "macos") {
                let mut definition = v2::OwnedDesktopEntryDefinition::MacosBundle {
                    app_leaf: "TheseusOwned.app".into(),
                    bundle_identifier: "dev.theseus.owned.fixture".into(),
                    executable_leaf: "reader".into(),
                    info_plist: v2::NativeArtifactCommitment {
                        length: 1,
                        sha256: "01".repeat(32),
                    },
                };
                let bytes = installer::native_entry_bytes(
                    &definition,
                    manifest.desktop_executable.as_ref().unwrap(),
                )?;
                if let v2::OwnedDesktopEntryDefinition::MacosBundle { info_plist, .. } =
                    &mut definition
                {
                    info_plist.length = bytes.len() as u64;
                    info_plist.sha256 = format!("{:x}", Sha256::digest(&bytes));
                }
                definition
            } else {
                v2::OwnedDesktopEntryDefinition::WindowsShortcut
            });
        }
        let entry_leaf = if native_entry {
            if cfg!(target_os = "macos") {
                "TheseusOwned.app"
            } else {
                "app.lnk"
            }
        } else if cfg!(target_os = "linux") {
            "theseus-source.desktop"
        } else {
            "theseus-source-owned.json"
        };
        let bytes = installer::entry_bytes(&manifest)?;
        let entry_digest = format!("{:x}", Sha256::digest(&bytes));
        let slot = v2::ResourceSlot::new("desktop")?;
        manifest.desktop = Some(v2::broker::DesktopManifest {
            schema: 1,
            namespace_policy: "exclusive_quiescent".into(),
            core_manifest_digest: core.to_hex(),
            slot: slot.clone(),
            entry_root: root.join("entries").display().to_string(),
            entry: entry_leaf.into(),
            entry_digest: entry_digest.clone(),
            executable_digest: expectation.artifact_sha256.clone(),
            allowed_modes: vec![v2::broker::DesktopMode::OwnedExecutable],
            ready_endpoint: ready,
            close_endpoint: close,
            expectation_digest: expectation.digest()?.to_hex(),
        });
        manifest.artifact_slots = vec![v2::IntegrationArtifactSlot {
            slot,
            relative_path: manifest
                .desktop_owned_entry
                .as_ref()
                .map(|entry| entry.entry_relative_path(manifest.desktop.as_ref().unwrap()))
                .unwrap_or_else(|| entry_leaf.into()),
            length: bytes.len() as u64,
            sha256: entry_digest,
        }];
        if scenario.is_some() {
            for (role, artifact_role) in [
                (
                    v2::DesktopDefinitionRole::Base,
                    v2::DesktopArtifactRole::Base,
                ),
                (
                    v2::DesktopDefinitionRole::Target,
                    v2::DesktopArtifactRole::Target,
                ),
                (
                    v2::DesktopDefinitionRole::Configured,
                    v2::DesktopArtifactRole::Base,
                ),
            ] {
                let mut definition = v2::DesktopDefinition {
                    owned_entry: manifest.desktop_owned_entry.clone(),
                    owned_lifetime: manifest.desktop_owned_lifetime,
                    key: v2::DesktopDefinitionKey {
                        role,
                        artifact: artifact_role,
                    },
                    entry_length: bytes.len() as u64,
                    desktop: manifest.desktop.clone().unwrap(),
                    executable: manifest.desktop_executable.clone().unwrap(),
                    readiness: manifest.desktop_readiness.clone().unwrap(),
                    native: None,
                };
                let source = if artifact_role == v2::DesktopArtifactRole::Base {
                    &config["artifacts"]["base"]
                } else {
                    &config["artifacts"]["target"]
                };
                definition.executable.length = source["length"].as_u64().unwrap();
                definition.executable.sha256 = source["sha256"].as_str().unwrap().into();
                definition.readiness.role = role.as_str().into();
                definition.readiness.artifact = artifact_role.as_str().into();
                definition.readiness.artifact_length = definition.executable.length;
                definition.readiness.artifact_sha256 = definition.executable.sha256.clone();
                definition.readiness.version_literal =
                    if artifact_role == v2::DesktopArtifactRole::Base {
                        "base\n"
                    } else {
                        "target\n"
                    }
                    .into();
                definition.desktop.executable_digest = definition.executable.sha256.clone();
                definition.desktop.expectation_digest = definition.readiness.digest()?.to_hex();
                definition.validate()?;
                manifest.desktop_definitions.push(definition);
            }
            manifest.validate()?;
        }
        let integration = manifest.digest()?;
        let full = v2::integration_manifest_digest(core, integration);
        let principal = theseus_host::principal::current_principal()?;
        let principal = match principal {
            theseus_host::principal::CurrentPrincipal::Posix {
                effective_uid,
                effective_gid,
            } => v2::broker::BrokerPrincipal::Posix {
                effective_uid,
                effective_gid,
            },
            theseus_host::principal::CurrentPrincipal::Windows { token_user_sid } => {
                v2::broker::BrokerPrincipal::Windows { token_user_sid }
            }
        };
        let header = v2::broker::IntegrationBrokerHeader {
            linux_gio_qualification: None,
            macos_security_session_id: None,
            profile: "native_integration_broker_v1".into(),
            binding: binding.clone(),
            manifest_digest: full.to_hex(),
            workspace_marker: marker.clone(),
            os: match installer::current_os() {
                v2::Os::Linux => "linux",
                v2::Os::Macos => "macos",
                v2::Os::Windows => "windows",
            }
            .into(),
            target_id: "source-desktop-target".into(),
            ownership_id: "03".repeat(16),
            provider_epoch: epoch.to_hex(),
            session_id: "explicit-source-owned".into(),
            session_epoch: epoch.to_hex(),
            broker_endpoint: endpoint,
            broker_principal: principal.clone(),
            helper_principal: principal,
            display: v2::broker::SuppliedDisplay::SourceOwnedHeadless,
            allowed_modes: vec![v2::broker::DesktopMode::OwnedExecutable],
        };
        let context = v2::IntegrationRuntimeContext {
            linux_gio_qualification: None,
            installation_instance_digest: None,
            manager_platform: None,
            kind: "theseus_native_integration_context".into(),
            schema: 1,
            ownership_id: header.ownership_id.clone(),
            binding: binding.clone(),
            manifest_digest: full.to_hex(),
            workspace_marker: marker.clone(),
            provider_epoch: epoch.to_hex(),
            integration_manifest_digest: integration.to_hex(),
            manager_unique_name: None,
            namespace_receipt: None,
            desktop_broker_header: Some(header.clone()),
        };
        manifest.validate_context(&context, installer::current_os())?;
        config["schema"] = 3.into();
        config["profile"] = "native_integration_v1".into();
        config["core_manifest_digest"] = core.to_hex().into();
        config["integration_manifest_digest"] = integration.to_hex().into();
        config["integration_manifest"] = serde_json::to_value(&manifest)?;
        config["runtime_context"] = serde_json::to_value(&context)?;
        config["manifest_digest"] = full.to_hex().into();
        write(&config_path, &config)?;
        write(
            &broker_path,
            &json!({"kind":"theseus_native_desktop_broker","schema":1,"profile":"native_integration_broker_v1","ledger_root":root.join("broker-ledger"),"workspace_root":root.join("broker-workspace"),"header":header,"manifest":manifest,"source_parent_eof":true}),
        )?;
        let template = root.join("provider-template.json");
        write(
            &template,
            &theseus::native::model::ProviderReceipt {
                provider: "integration_fixture_provider".into(),
                invocation_id: "pending".into(),
                invocation_digest: "00".repeat(32),
                binding: binding.clone(),
                manifest_digest: full.to_hex(),
                workspace_marker: marker.clone(),
                reset_epoch: epoch.to_hex(),
                receipt: "pending".into(),
            },
        )?;
        let mut catalog: Value =
            serde_json::from_str(include_str!("../../examples/native/recipe.json"))?;
        catalog["os"] = installer::os_name(installer::current_os()).into();
        catalog["leaves"]["reader"] = reader_leaf.into();
        catalog["manifest_digest"] = core.to_hex().into();
        catalog["base"] = artifact(&base)?;
        catalog["target"] = artifact(&target)?;
        if let Some(scenario) = scenario {
            catalog["scenarios"] = json!([scenario]);
            let target_step = catalog["steps"]["install_target"].clone();
            if scenario == ScenarioKind::Upgrade {
                let mut base_step = target_step.clone();
                base_step["id"] = "install_base".into();
                base_step["program"] = "install_base".into();
                catalog["steps"]["install_base"] = base_step;
                let mut upgrade = target_step;
                upgrade["id"] = "upgrade".into();
                catalog["steps"]["upgrade"] = upgrade;
            } else if scenario == ScenarioKind::Reinstall {
                let mut reinstall = target_step;
                reinstall["id"] = "reinstall".into();
                catalog["steps"]["reinstall"] = reinstall;
            }
            let checks = catalog["checks"].as_array().unwrap();
            let target_check = checks
                .iter()
                .find(|c| c["checkpoint"] == "target_installed")
                .unwrap()
                .clone();
            let data_check = checks
                .iter()
                .find(|c| c["checkpoint"] == "configured")
                .unwrap()
                .clone();
            if scenario == ScenarioKind::Upgrade {
                let mut base = target_check;
                base["id"] = "base_version".into();
                base["checkpoint"] = "base_installed".into();
                base["command"]["id"] = "base_version".into();
                base["command"]["program"] = "check_base".into();
                base["command"]["stdout_literal"] = "base\n".into();
                catalog["checks"].as_array_mut().unwrap().push(base);
                let mut data = data_check;
                data["id"] = "upgrade_data".into();
                data["checkpoint"] = "target_installed".into();
                data["command"]["id"] = "upgrade_data".into();
                catalog["checks"].as_array_mut().unwrap().push(data);
            } else if scenario == ScenarioKind::Reinstall {
                for (mut check, id) in [
                    (target_check, "reinstalled_target"),
                    (data_check, "reinstalled_data"),
                ] {
                    check["id"] = id.into();
                    check["checkpoint"] = "reinstalled".into();
                    check["command"]["id"] = id.into();
                    catalog["checks"].as_array_mut().unwrap().push(check);
                }
            }
        }

        let requirements = IntegrationRequirements {
            service: None,
            desktop: Some(DesktopRequirement {
                resource: ResourceSlot::new("desktop")?,
                modes: vec![DesktopMode::OwnedExecutable],
                readiness: ReadinessRequirement {
                    kind: ReadyKind::LocalChallenge,
                    protocol: ReadyProtocol::TheseusFixtureReadyV1,
                    deadline_ms: 30000,
                },
                lifetime: if cfg!(target_os = "macos") {
                    LifetimeRequirement::OwnedPrimaryExited
                } else {
                    LifetimeRequirement::OwnedProcessTreeSettled
                },
            }),
        };
        write(
            &recipe_path,
            &json!({"kind":"theseus_native_recipe","schema":2,"integration_manifest_digest":integration.to_hex(),"catalog":catalog,"integrations":requirements}),
        )?;
        let recipe =
            ValidatedIntegrationRecipe::decode(&std::fs::read(&recipe_path)?, recipe_path)?;
        let launch = root.join("launch.json");
        write(
            &launch,
            &json!({"fixture":"source-owned-desktop-only","manifest_digest":full.to_hex()}),
        )?;
        let graph = root.join("graph.json");
        write(
            &graph,
            &json!({"kind":"theseus_native_integration_fixture_graph","schema":1,"recipe":pin(&recipe.path)?,"launch":pin(&launch)?,"controller":pin(&controller)?,"base":pin(&base)?,"target":pin(&target)?,"helper":pin(&helper)?,"config":pin(&config_path)?,"provider":pin(&sut)?,"provider_template":pin(&template)?,"provider_root":root.join("provider"),"binding":binding,"manifest_digest":full.to_hex(),"workspace_marker":marker,"provider_epoch":epoch.to_hex(),"core_manifest_digest":core.to_hex(),"integration_manifest_digest":integration.to_hex(),"service_authority":null,"desktop_authority":header.authority(),"source_mode":"source_owned_desktop","broker":pin(&broker)?,"broker_config":pin(&broker_path)?}),
        )?;
        example_inputs::capture(root)?;
        let operator = Operator::start(root.join("provider"), broker, broker_path);
        let output = root.join(".theseus-runs");
        Ok(Self {
            directory: Some(directory),
            root: fixture_root,
            sockets,
            operator: Some(operator),
            finished: false,
            controller,
            graph,
            launch,
            recipe,
            output,
            fault,
        })
    }
    pub fn finish(mut self) -> Result<()> {
        if let Some(operator) = &mut self.operator {
            operator.finish()?;
        }
        self.operator.take();
        self.finished = true;
        Ok(())
    }
}
impl Drop for DesktopFixture {
    fn drop(&mut self) {
        let result = self.operator.as_mut().map(Operator::finish).transpose();
        self.operator.take();
        if let Err(error) = result {
            eprintln!("original desktop broker cleanup uncertain: {error:#}");
        }
        if let Some(root) = self.directory.take() {
            let _ = root.keep();
            eprintln!("retained desktop fixture: {}", self.root.display());
        }
        if let Some(root) = self.sockets.take() {
            eprintln!("retained desktop endpoints: {}", root.keep().display());
        }
    }
}

pub fn assert_entry_fault(report: &NativeIntegrationReport, fault: Fault) -> Result<()> {
    let (phase, predicate, step) = match fault {
        Fault::MissingEntry => (
            IntegrationPhase::TargetInstalledInitial,
            IntegrationPredicate::DesktopEntryPresent,
            StepName::InstallTarget,
        ),
        Fault::LeakedEntry => (
            IntegrationPhase::Uninstalled,
            IntegrationPredicate::DesktopEntryAbsent,
            StepName::Uninstall,
        ),
        Fault::None => anyhow::bail!("negative fixture fault required"),
    };
    let command = report.catalog.scenarios[0]
        .stages
        .iter()
        .find(|s| s.step == Some(step))
        .context("original SUT step")?
        .commands
        .first()
        .context("original SUT command")?;
    ensure!(
        command.failure.is_none()
            && command.status.as_ref() == Some(&command.command.expected_status),
        "intended SUT must succeed before its independent entry failure"
    );
    ensure!(
        report.integrations[0]
            .checks
            .iter()
            .any(|c| c.phase == phase
                && c.predicate == predicate
                && c.origin == CheckOrigin::SutPostcondition
                && c.outcome == CheckOutcome::Failed
                && c.sut_operation.as_deref() == Some(command.operation_id.as_str())),
        "exact required failed entry postcondition"
    );
    if matches!(fault, Fault::MissingEntry) {
        ensure!(
            report.integrations[0]
                .desktops
                .iter()
                .all(|d| d.actions.is_empty()),
            "missing SUT entry grants no activation"
        );
    }
    Ok(())
}
fn assert_public_observations(report: &NativeIntegrationReport, rows: &[Value]) -> Result<()> {
    for desktop in report.integrations.iter().flat_map(|s| &s.desktops) {
        for observation in &desktop.observations {
            ensure!(
                rows.iter().any(|row| row["record_kind"] == "desktop"
                    && row["detail"]["observation"] == serde_json::to_value(observation).unwrap()),
                "exact bound desktop observation is public evidence"
            );
        }
    }
    for service in report.integrations.iter().flat_map(|s| &s.services) {
        for observation in &service.observations {
            ensure!(
                rows.iter().any(|row| row["record_kind"] == "service"
                    && row["detail"]["observation"] == serde_json::to_value(observation).unwrap()),
                "exact bound service observation is public alongside desktop evidence"
            );
        }
    }
    Ok(())
}
pub fn assert_saved_views(report: &NativeIntegrationReport, directory: &Path) -> Result<()> {
    report.validate()?;
    let theseus::engine::domain::StoredRun::NativeIntegration(saved) =
        theseus::native::storage::load_run(directory)?
    else {
        anyhow::bail!("saved integration family")
    };
    ensure!(&saved == report, "same saved desktop report");
    let public = theseus::cli::fixture_saved_native_projection(directory)?;
    ensure!(
        public["saved"]["schema_version"] == 2
            && public["saved"]["outcome"] == serde_json::to_value(report.outcome)?,
        "saved public schema/outcome"
    );
    assert_public_observations(
        report,
        public["records"]
            .as_array()
            .context("public desktop records")?,
    )?;
    let store = theseus::report::open_existing(directory)?;
    let (_, NativeJournalVersion::IntegrationV2(journal)) =
        theseus::native::storage::load_native_versioned(&store)?
    else {
        anyhow::bail!("paired journal")
    };
    if report.outcome == theseus::model::RunOutcome::Passed {
        ensure!(
            journal.releases.len() == journal.ownership.len()
                && !journal.releases.is_empty()
                && journal.releases.iter().all(|r| matches!(
                    r.completion,
                    theseus::native::model::Disposition::Complete { .. }
                )),
            "actual original helper Release must complete"
        );
        for integration in &report.integrations {
            if integration.desktops.iter().any(|desktop| {
                desktop.observations.iter().any(|observation| {
                    observation
                        .broker_state
                        .as_ref()
                        .is_some_and(|state| state.owned_lifetime.is_some())
                })
            }) {
                ensure!(
                    integration.owned_lifetime.is_some(),
                    "explicit original owned lifetime must reach the scenario projection"
                );
            }
            if let Some(owned) = &integration.owned_lifetime {
                ensure!(
                    owned.state.descendants == v2::broker::OwnedTreeState::Unavailable
                        && matches!(
                            integration.dispositions.descendant_scope,
                            theseus::native::model::Disposition::NotStarted
                        ),
                    "Mac structural tree unavailability stays separate from completion"
                );
                ensure!(public["records"].as_array().context("public records")?.iter().any(|row| row["detail"]["observation"]["broker_state"]["owned_lifetime"]["descendants"] == "unavailable"), "saved public observations retain unavailable tree fact");
            }
        }
    }
    let public_bytes = serde_json::to_string(&public)?;
    fn private_nonces(value: &Value, result: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (k, v) in map {
                    if k.ends_with("nonce")
                        && let Some(v) = v.as_str()
                    {
                        result.push(v.into());
                    }
                    private_nonces(v, result);
                }
            }
            Value::Array(values) => {
                for v in values {
                    private_nonces(v, result);
                }
            }
            _ => {}
        }
    }
    let mut nonces = vec![];
    private_nonces(
        &serde_json::to_value(&journal.integration_actions)?,
        &mut nonces,
    );
    for nonce in nonces {
        ensure!(
            !public_bytes.contains(&nonce),
            "private desktop nonce escaped public evidence"
        );
    }
    let mut app = theseus::tui::App::integration_offline(report.clone(), directory.into());
    ensure!(
        !app.active && app.recipe.is_none(),
        "offline view has no execution authority"
    );
    let mut rendered = String::new();
    for _ in 0..10 {
        for _ in 0..3 {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 100))?;
            terminal.draw(|frame| theseus::tui::render(frame, &app))?;
            for cell in terminal.backend().buffer().content() {
                rendered.push_str(cell.symbol());
            }
            theseus::tui::input(
                &mut app,
                crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char('n'),
                    crossterm::event::KeyModifiers::NONE,
                ),
            );
        }
        theseus::tui::input(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Right,
                crossterm::event::KeyModifiers::NONE,
            ),
        );
    }
    if report
        .integrations
        .iter()
        .any(|scenario| scenario.owned_lifetime.is_some())
    {
        ensure!(
            rendered.to_ascii_lowercase().contains("unavailable"),
            "TUI retains explicit unavailable owned descendant capability"
        );
    }
    std::fs::write(directory.join("desktop-tui-render.raw"), &rendered)?;
    ensure!(
        rendered.contains(&report.profile)
            && rendered.contains("Desktop")
            && rendered.contains("Readiness"),
        "real offline TUI desktop/readiness tabs"
    );
    theseus::tui::input(
        &mut app,
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('r'),
            crossterm::event::KeyModifiers::NONE,
        ),
    );
    ensure!(!app.start_requested, "offline desktop cannot launch");
    Ok(())
}
pub fn assert_live_evidence(
    server: &mut crate::frontend::Controller,
    instance: &str,
    handle: &str,
    status: &Value,
    report: &NativeIntegrationReport,
) -> Result<()> {
    let revision = status["data"]["run"]["saved"]["revision"]
        .as_str()
        .context("saved revision")?;
    let mut rows = vec![];
    for collection in [
        "run",
        "desktop",
        "services",
        "readiness",
        "integration_checks",
        "integration_dispositions",
    ] {
        let mut cursor = None;
        for _ in 0..256 {
            let mut args = json!({"server_instance":instance,"handle":handle,"revision":revision,"selector":{"kind":"catalog","collection":collection},"limit":1});
            if let Some(value) = cursor.take() {
                args["cursor"] = value;
            }
            let page = server.tool("run_evidence", args)?["data"].clone();
            ensure!(page["revision"] == revision, "evidence revision");
            for summary in page["records"].as_array().context("evidence records")? {
                let mut bytes = vec![];
                let mut detail_cursor = None;
                let mut complete = false;
                for _ in 0..256 {
                    let mut args = json!({"server_instance":instance,"handle":handle,"revision":revision,"selector":{"kind":"record","record_id":summary["record_id"]},"limit":256});
                    if let Some(value) = detail_cursor.take() {
                        args["cursor"] = value;
                    }
                    let part = server.tool("run_evidence", args)?["data"].clone();
                    ensure!(
                        part["revision"] == revision
                            && part["record_id"] == summary["record_id"]
                            && part["offset"].as_u64() == Some(bytes.len() as u64),
                        "same bounded record interval"
                    );
                    let chunk: Vec<u8> = serde_json::from_value(part["bytes"].clone())?;
                    ensure!(
                        !chunk.is_empty() && bytes.len() + chunk.len() <= 65536,
                        "bounded evidence bytes"
                    );
                    bytes.extend(chunk);
                    if part["next_cursor"].is_null() {
                        ensure!(
                            part["total_bytes"].as_u64() == Some(bytes.len() as u64),
                            "complete record bytes"
                        );
                        complete = true;
                        break;
                    }
                    detail_cursor = Some(part["next_cursor"].clone());
                }
                ensure!(complete, "bounded complete detail pagination");
                rows.push(json!({"record_kind":summary["record_kind"],"record_id":summary["record_id"],"detail":serde_json::from_slice::<Value>(&bytes)?}));
            }
            if page["next_cursor"].is_null() {
                break;
            }
            cursor = Some(page["next_cursor"].clone());
        }
    }
    assert_public_observations(report, &rows)?;
    Ok(())
}
