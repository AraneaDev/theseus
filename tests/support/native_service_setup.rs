//! Supplied-manager acceptance fixture. Missing qualification fails; no model fallback.
#[path = "native_example_inputs.rs"]
pub(crate) mod example_inputs;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::path::{Path, PathBuf};
use theseus::{
    model::ScenarioKind,
    native::{
        integration::{model::*, recipe::ValidatedIntegrationRecipe},
        recipe::NativeArtifact,
    },
};
use theseus_host::fs::HostDir;
use theseus_native_protocol::v2;
#[path = "native_service_creator_state.rs"]
pub mod creator_state;
#[path = "native_service_owner_bridge.rs"]
pub mod owner_bridge;
#[cfg(any(target_os = "macos", windows))]
#[path = "native_service_owner_operator.rs"]
mod owner_operator;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagerReceipt {
    kind: String,
    schema: u32,
    manager: ManagerKind,
    authority: v2::ServiceAuthority,
    manager_unique_name: String,
    namespace_receipt: String,
    #[serde(default)]
    native_fixture: Option<NativeFixtureReceipt>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeFixtureReceipt {
    root: PathBuf,
    ready_root: PathBuf,
    cooperative_instance: String,
}
fn validate_native_receipt(receipt: &ManagerReceipt) -> Result<()> {
    ensure!(
        receipt.kind == "theseus_supplied_manager_receipt" && receipt.schema == 1,
        "wrong supplied native receipt"
    );
    receipt.authority.validate()?;
    let native = receipt.native_fixture.as_ref().ok_or_else(|| {
        anyhow::anyhow!("explicit native_fixture roots and original installation instance required")
    })?;
    let platform = receipt.authority.platform();
    ensure!(
        matches!(
            (receipt.manager, platform),
            (ManagerKind::LaunchdUser, v2::ManagerPlatform::Launchd)
                | (ManagerKind::WindowsScm, v2::ManagerPlatform::Scm)
        ),
        "wrong native manager authority"
    );
    for path in [&native.root, &native.ready_root] {
        ensure!(
            path.to_str()
                .is_some_and(|p| v2::platform_absolute(p, platform)),
            "explicit canonical platform root required"
        );
    }
    ensure!(
        native.root != native.ready_root
            && !native.root.starts_with(&native.ready_root)
            && !native.ready_root.starts_with(&native.root),
        "independent fixture roots required"
    );
    ensure!(
        !native.cooperative_instance.is_empty()
            && native.cooperative_instance.len() <= 128
            && !native.cooperative_instance.chars().any(char::is_control),
        "original installation instance required"
    );
    ensure!(
        !receipt.manager_unique_name.is_empty()
            && !receipt.manager_unique_name.starts_with(':')
            && !receipt.namespace_receipt.is_empty(),
        "explicit native context reservation required"
    );
    match &receipt.authority {
        v2::ServiceAuthority::Launchd { qualification, .. } => ensure!(
            qualification.absence_profile.is_some(),
            "exact native label absence qualification required"
        ),
        v2::ServiceAuthority::Scm { preflight, .. } => {
            let workspace = native.root.join("workspace");
            ensure!(
                Path::new(&preflight.executable) == workspace.join("reader.exe")
                    && Path::new(&preflight.definition) == workspace.join("reader.service.json")
                    && Path::new(&preflight.data) == workspace.join("data")
                    && preflight.helper_sid != "S-1-5-18",
                "SCM exact private paths and distinct helper principal required"
            );
        }
        _ => anyhow::bail!("native authority required"),
    }
    Ok(())
}
#[allow(dead_code)]
pub fn qualify_native_receipt_bytes(bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() <= 65536,
        "bounded explicit manager receipt required"
    );
    validate_native_receipt(&serde_json::from_slice(bytes)?)
}
#[path = "native_service_acl.rs"]
mod acl;
#[allow(dead_code)]
pub fn validate_native_acl_policy(
    helper: &str,
    protected: bool,
    grants: &[(&str, u32, u8)],
) -> Result<()> {
    acl::validate_grants(helper, acl::Kind::Executable, protected, grants)
}
impl ManagerReceipt {
    fn authority_version(&self) -> &str {
        match &self.authority {
            v2::ServiceAuthority::Systemd { qualification, .. } => &qualification.version,
            v2::ServiceAuthority::Launchd { qualification, .. } => &qualification.os_version,
            v2::ServiceAuthority::Scm { qualification, .. } => &qualification.version,
        }
    }
}
#[derive(Clone, Copy)]
pub enum Fault {
    None,
    ConfigureFails,
    MissingService,
    LeakedService,
    WrongVersion,
    WrongData,
    WrongNonce,
    WrongPeer,
}
impl Fault {
    fn name(self) -> &'static str {
        match self {
            Self::None | Self::ConfigureFails => "none",
            Self::MissingService => "missing_service",
            Self::LeakedService => "leaked_service",
            Self::WrongVersion => "wrong_version",
            Self::WrongData => "wrong_data",
            Self::WrongNonce => "wrong_nonce",
            Self::WrongPeer => "wrong_peer",
        }
    }
}
pub struct EvidenceDirectory(Option<tempfile::TempDir>, Option<PathBuf>, Option<HostDir>);
impl EvidenceDirectory {
    fn new(directory: tempfile::TempDir) -> Self {
        Self(Some(directory), None, None)
    }
    fn supplied(path: &Path) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(path)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "supplied fresh real directory required"
        );
        ensure!(
            std::fs::read_dir(path)?.next().is_none(),
            "supplied fixture root must be empty"
        );
        let owner = HostDir::open(path, theseus_host::fs::RootPolicy::AllComponentsNoFollow)?;
        Ok(Self(None, Some(path.to_owned()), Some(owner)))
    }
    pub fn path(&self) -> &Path {
        if let (Some(path), Some(owner)) = (&self.1, &self.2) {
            let current = HostDir::open(path, theseus_host::fs::RootPolicy::AllComponentsNoFollow)
                .expect("supplied root must remain available through original authority");
            assert!(
                owner
                    .same_directory_object(&current)
                    .expect("original supplied root identity"),
                "supplied root replaced; no same-path adoption"
            );
        }
        self.0
            .as_ref()
            .map(|d| d.path())
            .or(self.1.as_deref())
            .expect("retained fixture directory")
    }
}
impl Drop for EvidenceDirectory {
    fn drop(&mut self) {
        if let Some(path) = &self.1 {
            eprintln!(
                "retained supplied service fixture evidence: {}",
                path.display()
            );
        }
        if let Some(directory) = self.0.take() {
            eprintln!(
                "retained service fixture evidence: {}",
                directory.keep().display()
            );
        }
    }
}
pub struct IntegrationFixture {
    #[cfg(any(target_os = "macos", windows))]
    original_creator: std::sync::Mutex<Option<owner_operator::Operator>>,
    pub root: EvidenceDirectory,
    pub ready_root: EvidenceDirectory,
    pub recipe: ValidatedIntegrationRecipe,
    pub recipe_path: PathBuf,
    pub graph_path: PathBuf,
    pub controller_path: PathBuf,
    pub output: PathBuf,
    pub authority: v2::ServiceAuthority,
}
fn binary(variable: &str) -> Result<PathBuf> {
    let path = PathBuf::from(
        std::env::var_os(variable)
            .ok_or_else(|| anyhow::anyhow!("required pinned fixture binary {variable}"))?,
    );
    ensure!(
        path.is_absolute() && path.is_file(),
        "fixture binary must be absolute existing file"
    );
    Ok(path)
}
fn artifact(path: &Path) -> Result<NativeArtifact> {
    let bytes = std::fs::read(path)?;
    Ok(NativeArtifact {
        length: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(bytes)),
    })
}
fn pin(path: &Path) -> Result<serde_json::Value> {
    {
        let artifact = artifact(path)?;
        Ok(serde_json::json!({"path":path,"sha256":artifact.sha256,"length":artifact.length}))
    }
}
fn write(path: &Path, value: &impl Serialize) -> Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
#[path = "native_service_definition.rs"]
mod definition;
use definition::definition_bytes;
impl IntegrationFixture {
    /// Select one ordinary scenario without changing the admitted helper/manager
    /// graph. Checkpoint checks are scenario-specific (initial target has no data).
    pub fn scenario_recipe(&self, scenario: ScenarioKind) -> Result<ValidatedIntegrationRecipe> {
        use theseus::{
            model::{Checkpoint, StepName},
            native::model::CatalogProgram,
        };
        let mut recipe = self.recipe.recipe.clone();
        let catalog = &mut recipe.catalog;
        catalog.scenarios = vec![scenario];
        let target = catalog
            .steps
            .get(&StepName::InstallTarget)
            .expect("fixture install target")
            .clone();
        if scenario == ScenarioKind::Upgrade {
            let mut base = target.clone();
            base.id = "install_base".into();
            base.program = CatalogProgram::InstallBase;
            catalog.steps.insert(StepName::InstallBase, base);
            let mut upgrade = target.clone();
            upgrade.id = "upgrade".into();
            catalog.steps.insert(StepName::Upgrade, upgrade);
        } else if scenario == ScenarioKind::Reinstall {
            let mut reinstall = target;
            reinstall.id = "reinstall".into();
            catalog.steps.insert(StepName::Reinstall, reinstall);
        }
        let target_check = catalog
            .checks
            .iter()
            .find(|c| c.checkpoint == Checkpoint::TargetInstalled)
            .unwrap()
            .clone();
        let data_check = catalog
            .checks
            .iter()
            .find(|c| c.checkpoint == Checkpoint::Configured)
            .unwrap()
            .clone();
        if scenario == ScenarioKind::Upgrade {
            let mut base = target_check.clone();
            base.id = "base_version".into();
            base.checkpoint = Checkpoint::BaseInstalled;
            base.command.id = "base_version".into();
            base.command.program = CatalogProgram::CheckBase;
            base.command.stdout_literal = Some("base\n".into());
            catalog.checks.push(base);
            let mut data = data_check;
            data.id = "upgrade_data".into();
            data.command.id = "upgrade_data".into();
            data.checkpoint = Checkpoint::TargetInstalled;
            catalog.checks.push(data);
        } else if scenario == ScenarioKind::Reinstall {
            let mut target = target_check;
            target.id = "reinstalled_target".into();
            target.command.id = "reinstalled_target".into();
            target.checkpoint = Checkpoint::Reinstalled;
            catalog.checks.push(target);
            let mut data = data_check;
            data.id = "reinstalled_data".into();
            data.command.id = "reinstalled_data".into();
            data.checkpoint = Checkpoint::Reinstalled;
            catalog.checks.push(data);
        }
        let service = recipe
            .integrations
            .service
            .as_mut()
            .expect("fixture service");
        service.definitions = vec![
            DefinitionKey {
                role: DefinitionRole::Base,
                artifact: ArtifactRole::Base,
            },
            DefinitionKey {
                role: DefinitionRole::Configured,
                artifact: ArtifactRole::Base,
            },
            DefinitionKey {
                role: DefinitionRole::Target,
                artifact: ArtifactRole::Target,
            },
            DefinitionKey {
                role: DefinitionRole::Configured,
                artifact: ArtifactRole::Target,
            },
        ];
        let (initial, initial_role, initial_artifact) = if scenario == ScenarioKind::Upgrade {
            (
                IntegrationPhase::BaseInstalled,
                DefinitionRole::Base,
                ArtifactRole::Base,
            )
        } else {
            (
                IntegrationPhase::TargetInstalledInitial,
                DefinitionRole::Target,
                ArtifactRole::Target,
            )
        };
        let mut rows = vec![
            (
                IntegrationPhase::Baseline,
                None,
                None,
                vec![ServicePredicate::RegistrationAbsent],
            ),
            (
                initial,
                Some(initial_role),
                Some(initial_artifact),
                vec![
                    ServicePredicate::Registration,
                    ServicePredicate::Configuration,
                    ServicePredicate::Stopped,
                ],
            ),
            (
                IntegrationPhase::Configured,
                Some(DefinitionRole::Configured),
                Some(initial_artifact),
                vec![
                    ServicePredicate::Configuration,
                    ServicePredicate::Running,
                    ServicePredicate::ApplicationReady,
                ],
            ),
            (
                IntegrationPhase::Uninstalled,
                None,
                None,
                vec![ServicePredicate::RegistrationAbsent],
            ),
        ];
        if service.controls.uninstall_precondition == RuntimePrecondition::Running {
            rows.push((
                IntegrationPhase::Uninstalled,
                None,
                None,
                vec![ServicePredicate::Stopped],
            ));
        }
        let exercises = if scenario == ScenarioKind::Reinstall {
            vec![
                IntegrationPhase::ExerciseBeforeReinstall,
                IntegrationPhase::ExerciseAfterReinstall,
            ]
        } else {
            vec![IntegrationPhase::ExerciseFinal]
        };
        for phase in exercises {
            rows.push((
                phase,
                Some(DefinitionRole::Configured),
                Some(ArtifactRole::Target),
                vec![
                    ServicePredicate::Running,
                    ServicePredicate::ApplicationReady,
                ],
            ));
        }
        let transition = match scenario {
            ScenarioKind::Upgrade => Some(IntegrationPhase::TargetInstalledAfterUpgrade),
            ScenarioKind::Reinstall => Some(IntegrationPhase::Reinstalled),
            _ => None,
        };
        if let Some(phase) = transition {
            rows.push((
                phase,
                Some(DefinitionRole::Configured),
                Some(ArtifactRole::Target),
                vec![
                    ServicePredicate::Registration,
                    ServicePredicate::Configuration,
                    ServicePredicate::Running,
                    ServicePredicate::ApplicationReady,
                ],
            ));
        }
        service.checks.clear();
        for (phase, role, artifact, predicates) in rows {
            for predicate in predicates {
                service.checks.push(ServiceCheckRequirement {
                    id: format!("{scenario:?}-{phase:?}-{predicate:?}"),
                    scenarios: vec![scenario],
                    phase,
                    role,
                    artifact,
                    predicate,
                });
            }
        }
        let bytes = serde_json::to_vec_pretty(&recipe)?;
        let validated = ValidatedIntegrationRecipe::decode(&bytes, self.recipe_path.clone())?;
        std::fs::write(&self.recipe_path, bytes)?;
        Ok(validated)
    }
    #[cfg(any(target_os = "macos", windows))]
    pub fn native_input(&self) -> Result<(v2::IntegrationManifest, v2::IntegrationRuntimeContext)> {
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.root.path().join("config.json"))?)?;
        Ok((
            serde_json::from_value(value["integration_manifest"].clone())?,
            serde_json::from_value(value["runtime_context"].clone())?,
        ))
    }
    #[cfg(any(target_os = "macos", windows))]
    pub fn native_sut_action(&self, action: &str) -> Result<String> {
        let extension = if cfg!(windows) { ".exe" } else { "" };
        let artifact = |name: &str| {
            self.root
                .path()
                .join("artifacts")
                .join(format!("{name}{extension}"))
        };
        let output = std::process::Command::new(artifact("provider"))
            .arg(action)
            .arg(self.root.path().join("config.json"))
            .arg(artifact("base"))
            .arg(artifact("target"))
            .arg("none")
            .arg(self.root.path().join("native-operator"))
            .output()?;
        ensure!(
            output.status.success(),
            "SUT {action} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut commitment = Sha256::new();
        commitment.update(action.as_bytes());
        commitment.update(std::fs::read(self.root.path().join("config.json"))?);
        commitment.update(output.stdout);
        commitment.update(output.stderr);
        Ok(format!(
            "fixture-observed-catalog-{action}-{:x}",
            commitment.finalize()
        ))
    }
    #[cfg(any(target_os = "macos", windows))]
    pub fn finish_native_fixture(&self) -> Result<bool> {
        if let Some(operator) = self
            .original_creator
            .lock()
            .map_err(|_| anyhow::anyhow!("original creator lock poisoned"))?
            .as_mut()
        {
            return operator.finish();
        }
        Ok(false)
    }
    pub fn place_preexisting_definition(&self) -> Result<(PathBuf, Vec<u8>)> {
        use std::io::Write;
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.root.path().join("config.json"))?)?;
        let manifest: v2::IntegrationManifest =
            serde_json::from_value(config["integration_manifest"].clone())?;
        let definition = manifest
            .definitions
            .iter()
            .find(|d| d.role == "target" && d.artifact == "target")
            .ok_or_else(|| anyhow::anyhow!("target definition missing"))?;
        let bytes = definition_bytes(definition);
        let v2::ServiceAuthority::Systemd {
            definition_root,
            unit,
            ..
        } = &self.authority
        else {
            anyhow::bail!("systemd fixture authority required")
        };
        let path = Path::new(definition_root).join(unit);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok((path, bytes))
    }
    pub fn supplied_manager(manager: ManagerKind, fault: Fault) -> Result<Self> {
        ensure!(
            !(manager == ManagerKind::WindowsScm && matches!(fault, Fault::WrongPeer)),
            "SCM wrong-peer full lifecycle requires a separately supplied real client token; use the native pipe principal-refusal fixture, never a passing SYSTEM substitution"
        );
        let variable = match manager {
            ManagerKind::SystemdUser if cfg!(target_os = "linux") => {
                "THESEUS_NATIVE_SYSTEMD_RECEIPT"
            }
            ManagerKind::LaunchdUser if cfg!(target_os = "macos") => {
                "THESEUS_NATIVE_LAUNCHD_RECEIPT"
            }
            ManagerKind::WindowsScm if cfg!(windows) => "THESEUS_NATIVE_SCM_RECEIPT",
            _ => anyhow::bail!("supplied native manager must match actual host OS"),
        };
        let inventory_variable = format!("{variable}_INVENTORY");
        let receipt_path = if manager != ManagerKind::SystemdUser
            && std::env::var_os(&inventory_variable).is_some()
        {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            static PATHS: std::sync::OnceLock<std::result::Result<Vec<PathBuf>, String>> =
                std::sync::OnceLock::new();
            let paths = PATHS.get_or_init(|| {
                (|| -> Result<Vec<PathBuf>> {
                    let path = PathBuf::from(
                        std::env::var_os(&inventory_variable).expect("explicit inventory"),
                    );
                    let bytes = std::fs::read(path)?;
                    ensure!(bytes.len() <= 65536, "bounded receipt inventory required");
                    let paths: Vec<PathBuf> = serde_json::from_slice(&bytes)?;
                    ensure!(
                        !paths.is_empty()
                            && paths.len() <= 128
                            && paths.iter().all(|p| p.is_absolute()),
                        "explicit absolute receipt paths required"
                    );
                    ensure!(
                        paths
                            .iter()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            == paths.len(),
                        "receipt inventory cannot reuse a reservation"
                    );
                    Ok(paths)
                })()
                .map_err(|e| e.to_string())
            });
            let paths = paths
                .as_ref()
                .map_err(|e| anyhow::anyhow!("invalid supplied receipt inventory: {e}"))?;
            paths.get(NEXT.fetch_add(1,std::sync::atomic::Ordering::SeqCst)).cloned().ok_or_else(||anyhow::anyhow!("explicit native fixture receipt inventory exhausted; fresh independent authority required"))?
        } else {
            PathBuf::from(std::env::var_os(variable).ok_or_else(|| {
                anyhow::anyhow!(
                    "real explicit {variable} qualification is required; no model or skip fallback"
                )
            })?)
        };
        let receipt_bytes = std::fs::read(receipt_path)?;
        ensure!(
            receipt_bytes.len() <= 65536,
            "bounded manager receipt required"
        );
        let receipt: ManagerReceipt = serde_json::from_slice(&receipt_bytes)?;
        if manager != ManagerKind::SystemdUser {
            validate_native_receipt(&receipt)?;
        }
        ensure!(
            !receipt.authority_version().contains("model"),
            "a model receipt cannot satisfy supplied-manager acceptance"
        );
        Self::from_receipt(receipt, manager, fault, "theseus_supplied_manager_receipt")
    }
    pub fn model_manager(receipt_path: &Path, fault: Fault) -> Result<Self> {
        let receipt: ManagerReceipt = serde_json::from_slice(&std::fs::read(receipt_path)?)?;
        ensure!(
            receipt.authority_version().contains("model"),
            "local model fixture must be labeled model"
        );
        Self::from_receipt(
            receipt,
            ManagerKind::SystemdUser,
            fault,
            "theseus_model_manager_receipt",
        )
    }
    fn from_receipt(
        mut receipt: ManagerReceipt,
        manager: ManagerKind,
        fault: Fault,
        expected_kind: &str,
    ) -> Result<Self> {
        ensure!(
            receipt.kind == expected_kind && receipt.schema == 1 && receipt.manager == manager,
            "wrong manager fixture qualification"
        );
        receipt.authority.validate()?;
        let native = receipt.native_fixture.clone();
        let platform = receipt.authority.platform();
        let (root, ready_root) = if let Some(native) = &native {
            validate_native_receipt(&receipt)?;
            (
                EvidenceDirectory::supplied(&native.root)?,
                EvidenceDirectory::supplied(&native.ready_root)?,
            )
        } else {
            ensure!(
                platform == v2::ManagerPlatform::Systemd,
                "native receipt layout required"
            );
            (
                EvidenceDirectory::new(tempfile::tempdir()?),
                EvidenceDirectory::new(
                    tempfile::Builder::new()
                        .prefix("th-ready-")
                        .tempdir_in("/tmp")?,
                ),
            )
        };
        #[cfg(windows)]
        if let v2::ServiceAuthority::Scm { preflight, .. } = &receipt.authority {
            definition::verify_current_sid(&preflight.helper_sid)?;
            for directory in [root.path(), ready_root.path()] {
                acl::verify_path(directory, &preflight.helper_sid, acl::Kind::Directory)?;
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for directory in [root.path(), ready_root.path()] {
                if native.is_none() {
                    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
                }
                ensure!(
                    std::fs::metadata(directory)?.permissions().mode() & 0o077 == 0,
                    "fixture directory must be private before any helper effect"
                );
            }
        }
        #[cfg(target_os = "macos")]
        if let v2::ServiceAuthority::Launchd {
            domain: v2::LaunchdDomain::User { uid },
            ..
        } = &receipt.authority
        {
            ensure!(
                rustix::process::geteuid().as_raw() == *uid,
                "actual fixture principal differs from supplied launchd User authority"
            );
        }
        let workspace = root.path().join("workspace");
        let ledger = root.path().join("ledger");
        let provider_root = root.path().join("provider");
        for path in [&workspace, &ledger, &provider_root] {
            HostDir::create_private(path)?;
        }
        #[cfg(any(target_os = "macos", windows))]
        let operator_root = root.path().join("native-operator");
        #[cfg(any(target_os = "macos", windows))]
        let operator_directory = HostDir::create_private(&operator_root)?;
        let artifacts = root.path().join("artifacts");
        HostDir::create_private(&artifacts)?;
        #[cfg(windows)]
        if let v2::ServiceAuthority::Scm { preflight, .. } = &receipt.authority {
            for directory in [&workspace, &ledger, &provider_root, &artifacts] {
                acl::initialize_created_path(
                    directory,
                    &preflight.helper_sid,
                    acl::Kind::Directory,
                )?;
            }
            acl::initialize_created_path(
                &operator_root,
                &preflight.helper_sid,
                acl::Kind::Directory,
            )?;
            acl::probe_install_paths(preflight)?;
        }
        let copy = |variable: &str, name: &str| -> Result<PathBuf> {
            let source = binary(variable)?;
            let destination = artifacts.join(if platform == v2::ManagerPlatform::Scm {
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
            #[cfg(not(unix))]
            {
                let mut permissions = std::fs::metadata(&destination)?.permissions();
                permissions.set_readonly(true);
                std::fs::set_permissions(&destination, permissions)?;
            }
            #[cfg(windows)]
            if let v2::ServiceAuthority::Scm { preflight, .. } = &receipt.authority {
                acl::initialize_created_path(
                    &destination,
                    &preflight.helper_sid,
                    acl::Kind::Executable,
                )?;
            }
            Ok(destination)
        };
        let helper = copy("THESEUS_NATIVE_HELPER_BINARY", "helper")?;
        let sut = copy("THESEUS_NATIVE_SERVICE_FIXTURE", "provider")?;
        let base_path = copy("THESEUS_NATIVE_SERVICE_BASE", "base")?;
        let target_path = copy("THESEUS_NATIVE_SERVICE_TARGET", "target")?;
        let controller_path = copy("THESEUS_NATIVE_INTEGRATION_CONTROLLER", "controller")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&artifacts, std::fs::Permissions::from_mode(0o500))?;
        }
        let base = artifact(&base_path)?;
        let target = artifact(&target_path)?;
        let config_path = root.path().join("config.json");
        let template_path = root.path().join("provider-template.json");
        let reader = workspace.join(if platform == v2::ManagerPlatform::Scm {
            "reader.exe"
        } else {
            "reader"
        });
        let data = workspace.join("data");
        let socket = ready_root.path().join("ready");
        let mut random = [0; 32];
        theseus_host::random_bytes(&mut random)?;
        let epoch = v2::Digest::from_bytes(random);
        let unit = format!("theseus-{}.service", &epoch.to_hex()[..20]);
        if let v2::ServiceAuthority::Systemd {
            unit: unit_name, ..
        } = &mut receipt.authority
        {
            *unit_name = unit;
        }
        let instance = native.as_ref().map(|n| n.cooperative_instance.clone());
        let endpoint = match &receipt.authority {
            v2::ServiceAuthority::Scm { preflight, .. } => v2::LocalEndpoint::NamedPipe {
                path: preflight.pipe.clone(),
            },
            _ => v2::LocalEndpoint::UnixPath {
                path: socket.display().to_string(),
            },
        };
        let os = match platform {
            v2::ManagerPlatform::Systemd => "linux",
            v2::ManagerPlatform::Launchd => "macos",
            v2::ManagerPlatform::Scm => "windows",
        };
        let ids = [
            "check_base",
            "check_data",
            "check_target",
            "configure",
            "exercise",
            "install_base",
            "install_target",
            "uninstall",
        ];
        let programs=ids.iter().map(|id|{
            let installer_fault = if *id == "configure" && matches!(fault, Fault::ConfigureFails) { "configure_fails" } else { fault.name() };
            let check=matches!(*id,"check_base"|"check_data"|"check_target"|"exercise");
            let args = if check { vec![id.to_string(),data.display().to_string()] } else {
                let args = vec![id.to_string(),config_path.display().to_string(),base_path.display().to_string(),target_path.display().to_string(),installer_fault.into()];
                #[cfg(any(target_os = "macos", windows))]
                let args = { let mut args = args; args.push(operator_root.display().to_string()); args };
                args
            };
            serde_json::json!({"id":id,"program":if check{&reader}else{&sut},"args":args,"environment":{"kind":"replace","entries":[]}})
        }).collect::<Vec<_>>();
        let mut config = serde_json::json!({"schema":2,"profile":"native_catalog_v1","capabilities":v2::NativeCapabilities::NAMES,"provider_epoch":epoch.to_hex(),"artifacts":{"base":base,"target":target},"ledger_root":ledger,"workspace_root":workspace,"binding":"01".repeat(16),"manifest_digest":"00".repeat(32),"workspace_marker":"02".repeat(16),"os":os,"programs":programs});
        let core = theseus_native_helper::config_manifest_digest(&serde_json::to_vec(&config)?)?;
        let definitions = [
            ("base", "base"),
            ("configured", "base"),
            ("target", "target"),
            ("configured", "target"),
        ]
        .into_iter()
        .map(|(role, artifact)| {
            let mut d = v2::ServiceDefinition {
                scm: None,
                role: role.into(),
                artifact: artifact.into(),
                executable: reader.display().to_string(),
                arguments: vec![
                    reader.display().to_string(),
                    "service".into(),
                    socket.display().to_string(),
                    data.display().to_string(),
                    fault.name().into(),
                ],
                environment: vec![format!("THESEUS_FIXTURE_DEFINITION={role}-{artifact}")],
                working_directory: workspace.display().to_string(),
                definition_sha256: "00".repeat(32),
            };
            if platform != v2::ManagerPlatform::Systemd {
                d.environment.clear();
            }
            d.definition_sha256 = match &receipt.authority {
                v2::ServiceAuthority::Systemd { .. } => {
                    format!("{:x}", Sha256::digest(definition_bytes(&d)))
                }
                v2::ServiceAuthority::Launchd { label, .. } => {
                    d.arguments.push(instance.clone().expect("native instance"));
                    format!(
                        "{:x}",
                        Sha256::digest(definition::launchd_definition_bytes(label, &d))
                    )
                }
                v2::ServiceAuthority::Scm { service_name, .. } => {
                    d.arguments = vec![d.executable.clone()];
                    let raw = native_scm_definition(
                        &d.executable,
                        &format!("{service_name} {role}-{artifact}"),
                    );
                    let digest = raw.digest().expect("closed SCM definition").to_hex();
                    d.scm = Some(raw);
                    digest
                }
            };
            d
        })
        .collect::<Vec<_>>();
        let data_digest = format!("{:x}", Sha256::digest(b"theseus fixture data\n"));
        let expectations = definitions
            .iter()
            .map(|d| {
                let artifact = if d.artifact == "base" { &base } else { &target };
                v2::ReadinessDefinitionExpectation {
                    role: d.role.clone(),
                    artifact: d.artifact.clone(),
                    artifact_length: artifact.length,
                    artifact_sha256: artifact.sha256.clone(),
                    version_literal: format!("{}\n", d.artifact),
                    data_sha256: data_digest.clone(),
                    configuration_digest: data_digest.clone(),
                }
            })
            .collect();
        let peer_uid = match &receipt.authority {
            v2::ServiceAuthority::Systemd {
                expected_manager_uid,
                ..
            } => Some(*expected_manager_uid),
            v2::ServiceAuthority::Launchd {
                domain: v2::LaunchdDomain::User { uid },
                ..
            } => Some(*uid),
            v2::ServiceAuthority::Scm { .. } => None,
            _ => anyhow::bail!("explicit user domain required"),
        };
        #[cfg(unix)]
        let gid = rustix::process::getegid().as_raw();
        #[cfg(not(unix))]
        let gid = 0;
        let manifest = v2::IntegrationManifest {
            desktop_owned_entry: None,
            desktop_owned_lifetime: None,
            desktop_native: None,
            desktop_definitions: Vec::new(),
            schema: 1,
            core_manifest_digest: core.to_hex(),
            service_authority: Some(receipt.authority.clone()),
            desktop: None,
            desktop_executable: None,
            desktop_readiness: None,
            definitions,
            artifact_slots: vec![],
            readiness_protocol: "theseus_fixture_ready_v1".into(),
            desktop_capabilities: vec![],
            readiness: Some(v2::IntegrationReadiness {
                peer_sid: if platform == v2::ManagerPlatform::Scm {
                    Some("S-1-5-18".into())
                } else {
                    None
                },
                cooperative_instance: instance.clone(),
                endpoint,
                root: ready_root.path().display().to_string(),
                peer_uid: peer_uid.map(|uid| {
                    if matches!(fault, Fault::WrongPeer) {
                        uid.checked_add(1).expect("peer UID")
                    } else {
                        uid
                    }
                }),
                peer_gid: peer_uid.map(|_| gid),
                expectations,
            }),
        };
        let integration = manifest.digest()?;
        let full = v2::integration_manifest_digest(core, integration);
        let context = v2::IntegrationRuntimeContext {
            linux_gio_qualification: None,
            manager_platform: Some(platform),
            desktop_broker_header: None,
            installation_instance_digest: instance
                .as_ref()
                .map(|value| format!("{:x}", Sha256::digest(value.as_bytes()))),
            kind: "theseus_native_integration_context".into(),
            schema: 1,
            ownership_id: "provider-reservation-pending".into(),
            binding: "01".repeat(16),
            manifest_digest: full.to_hex(),
            workspace_marker: "02".repeat(16),
            provider_epoch: epoch.to_hex(),
            integration_manifest_digest: integration.to_hex(),
            manager_unique_name: Some(receipt.manager_unique_name),
            namespace_receipt: Some(receipt.namespace_receipt),
        };
        context.validate()?;
        config["schema"] = 3.into();
        config["profile"] = "native_integration_v1".into();
        config["capabilities"] = serde_json::json!(v2::NativeCapabilities::NAMES);
        config["core_manifest_digest"] = core.to_hex().into();
        config["integration_manifest_digest"] = integration.to_hex().into();
        config["integration_manifest"] = serde_json::to_value(&manifest)?;
        config["runtime_context"] = serde_json::to_value(&context)?;
        config["manifest_digest"] = full.to_hex().into();
        write(&config_path, &config)?;
        write(
            &template_path,
            &theseus::native::model::ProviderReceipt {
                provider: "integration_fixture_provider".into(),
                invocation_id: "pending".into(),
                invocation_digest: "00".repeat(32),
                binding: context.binding.clone(),
                manifest_digest: full.to_hex(),
                workspace_marker: context.workspace_marker.clone(),
                reset_epoch: epoch.to_hex(),
                receipt: "pending".into(),
            },
        )?;
        let mut catalog: serde_json::Value =
            serde_json::from_str(include_str!("../../examples/native/recipe.json"))?;
        catalog["os"] = os.into();
        if platform == v2::ManagerPlatform::Scm {
            catalog["leaves"]["reader"] = "reader.exe".into();
            for step in catalog["steps"]
                .as_object_mut()
                .expect("catalog steps")
                .values_mut()
            {
                step["expected_status"] = serde_json::json!({"kind":"windows_exit","code":0});
            }
            for check in catalog["checks"].as_array_mut().expect("catalog checks") {
                if check["command"].get("expected_status").is_some() {
                    check["command"]["expected_status"] =
                        serde_json::json!({"kind":"windows_exit","code":0});
                }
            }
        }
        catalog["manifest_digest"] = core.to_hex().into();
        catalog["base"] = serde_json::to_value(&base)?;
        catalog["target"] = serde_json::to_value(&target)?;
        let key = DefinitionKey {
            role: DefinitionRole::Configured,
            artifact: ArtifactRole::Target,
        };
        let checks = [
            (
                "installed_stopped",
                IntegrationPhase::TargetInstalledInitial,
                Some(DefinitionKey {
                    role: DefinitionRole::Target,
                    artifact: ArtifactRole::Target,
                }),
                ServicePredicate::Stopped,
            ),
            (
                "exercise_running",
                IntegrationPhase::ExerciseFinal,
                Some(DefinitionKey {
                    role: DefinitionRole::Configured,
                    artifact: ArtifactRole::Target,
                }),
                ServicePredicate::Running,
            ),
            (
                "baseline_absent",
                IntegrationPhase::Baseline,
                None,
                ServicePredicate::RegistrationAbsent,
            ),
            (
                "exercise_ready",
                IntegrationPhase::ExerciseFinal,
                Some(DefinitionKey {
                    role: DefinitionRole::Configured,
                    artifact: ArtifactRole::Target,
                }),
                ServicePredicate::ApplicationReady,
            ),
            (
                "sut_stopped",
                IntegrationPhase::Uninstalled,
                None,
                ServicePredicate::Stopped,
            ),
            (
                "installed_registration",
                IntegrationPhase::TargetInstalledInitial,
                Some(DefinitionKey {
                    role: DefinitionRole::Target,
                    artifact: ArtifactRole::Target,
                }),
                ServicePredicate::Registration,
            ),
            (
                "installed_configuration",
                IntegrationPhase::TargetInstalledInitial,
                Some(DefinitionKey {
                    role: DefinitionRole::Target,
                    artifact: ArtifactRole::Target,
                }),
                ServicePredicate::Configuration,
            ),
            (
                "configured_configuration",
                IntegrationPhase::Configured,
                Some(key.clone()),
                ServicePredicate::Configuration,
            ),
            (
                "configured_running",
                IntegrationPhase::Configured,
                Some(key.clone()),
                ServicePredicate::Running,
            ),
            (
                "configured_ready",
                IntegrationPhase::Configured,
                Some(key),
                ServicePredicate::ApplicationReady,
            ),
            (
                "removed_registration",
                IntegrationPhase::Uninstalled,
                None,
                ServicePredicate::RegistrationAbsent,
            ),
        ]
        .into_iter()
        .map(|(id, phase, key, predicate)| ServiceCheckRequirement {
            id: id.into(),
            scenarios: vec![ScenarioKind::Clean],
            phase,
            role: key.as_ref().map(|k| k.role),
            artifact: key.map(|k| k.artifact),
            predicate,
        })
        .collect();
        let requirements = IntegrationRequirements {
            service: Some(ServiceRequirement {
                resource: ResourceSlot::new("service")?,
                managed_by: ManagedBy::Installer,
                controls: RehearsalControls {
                    configured_activation: ActivationCause::RehearsalStart,
                    upgrade_precondition: RuntimePrecondition::Running,
                    reinstall_precondition: RuntimePrecondition::Running,
                    uninstall_precondition: RuntimePrecondition::Running,
                },
                definitions: vec![
                    DefinitionKey {
                        role: DefinitionRole::Target,
                        artifact: ArtifactRole::Target,
                    },
                    DefinitionKey {
                        role: DefinitionRole::Configured,
                        artifact: ArtifactRole::Target,
                    },
                ],
                checks,
                readiness: ReadinessRequirement {
                    kind: ReadyKind::LocalChallenge,
                    protocol: ReadyProtocol::TheseusFixtureReadyV1,
                    deadline_ms: 30000,
                },
            }),
            desktop: None,
        };
        let recipe_path = root.path().join("recipe.json");
        write(
            &recipe_path,
            &serde_json::json!({"kind":"theseus_native_recipe","schema":2,"integration_manifest_digest":integration.to_hex(),"catalog":catalog,"integrations":requirements}),
        )?;
        let recipe =
            ValidatedIntegrationRecipe::decode(&std::fs::read(&recipe_path)?, recipe_path.clone())?;
        let launch_path = root.path().join("launch.json");
        write(
            &launch_path,
            &serde_json::json!({"fixture":"supplied-manager-only","manifest_digest":full.to_hex()}),
        )?;
        let graph_path = root.path().join("graph.json");
        write(
            &graph_path,
            &serde_json::json!({"kind":"theseus_native_integration_fixture_graph","schema":1,"recipe":pin(&recipe_path)?,"launch":pin(&launch_path)?,"controller":pin(&controller_path)?,"base":pin(&base_path)?,"target":pin(&target_path)?,"helper":pin(&helper)?,"config":pin(&config_path)?,"provider":pin(&sut)?,"provider_template":pin(&template_path)?,"provider_root":provider_root,"binding":context.binding,"manifest_digest":full.to_hex(),"workspace_marker":context.workspace_marker,"provider_epoch":epoch.to_hex(),"core_manifest_digest":core.to_hex(),"integration_manifest_digest":integration.to_hex(),"service_authority":receipt.authority}),
        )?;
        example_inputs::capture(root.path())?;
        let output = root.path().join(".theseus-runs");
        #[cfg(any(target_os = "macos", windows))]
        let original_creator = std::sync::Mutex::new(Some(owner_operator::Operator::start(
            operator_root,
            operator_directory,
            manifest,
            context,
            fault.name().into(),
        )?));
        Ok(Self {
            #[cfg(any(target_os = "macos", windows))]
            original_creator,
            root,
            ready_root,
            recipe,
            recipe_path,
            graph_path,
            controller_path,
            output,
            authority: receipt.authority,
        })
    }
}

#[cfg(any(target_os = "macos", windows))]
impl Drop for IntegrationFixture {
    fn drop(&mut self) {
        if let Err(error) = self.finish_native_fixture() {
            eprintln!("separate fixture original-creator teardown failed: {error:#}");
        }
    }
}

fn native_scm_definition(executable: &str, display: &str) -> v2::ScmDefinition {
    v2::ScmDefinition {
        command_line: format!("\"{executable}\"").encode_utf16().collect(),
        display_name: display.encode_utf16().collect(),
        account: "LocalSystem".encode_utf16().collect(),
        service_type: 0x10,
        start_type: 3,
        error_control: 1,
        load_order_group: vec![],
        dependencies: vec![],
        tag_id: 0,
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
    }
}
