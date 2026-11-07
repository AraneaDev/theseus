//! Candidate/pinned binary setup shared by native selected-engine tests.
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, path::PathBuf};
use theseus::{
    model::{Checkpoint, ScenarioKind, StepName},
    native::{
        launch::LaunchAuthority,
        model::{CatalogCommand, CatalogProgram, NativeStatus},
        recipe::{
            NativeArtifact, NativeCheck, NativeLeaves, NativeOs, NativeRecipe, NativeRunLimits,
            ValidatedNativeRecipe,
        },
    },
};
use theseus_host::{
    fs::HostDir,
    process::{ChildEnvironment, NativeCommand},
};
use theseus_native_helper_store::{BootstrapRequest, StoreBinding, bootstrap_native};
use theseus_native_protocol::v2;
pub struct NativeCatalogFixture {
    pub root: tempfile::TempDir,
    pub workspace: PathBuf,
    pub config: PathBuf,
    pub digest: v2::Digest,
    pub binding: v2::BindingId,
    pub marker: v2::WorkspaceMarker,
    pub epoch: v2::Digest,
    pub base: NativeArtifact,
    pub target: NativeArtifact,
    helper: PathBuf,
    installer: PathBuf,
    provider_root: PathBuf,
    template: PathBuf,
}
fn binary(name: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(name).unwrap_or_else(|| panic!("required explicit inert binary {name}")),
    );
    assert!(path.is_absolute() && path.is_file());
    path
}
fn artifact(path: &PathBuf) -> NativeArtifact {
    let bytes = std::fs::read(path).unwrap();
    NativeArtifact {
        length: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(bytes)),
    }
}
impl NativeCatalogFixture {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let workspace_dir = HostDir::create_private(&workspace).unwrap();
        let ledger_path = root.path().join("ledger");
        let ledger = HostDir::create_private(&ledger_path).unwrap();
        let installer = binary("THESEUS_NATIVE_CATALOG_FIXTURE");
        let base_path = binary("THESEUS_NATIVE_BASE_READER");
        let target_path = binary("THESEUS_NATIVE_TARGET_READER");
        let helper = binary("THESEUS_NATIVE_HELPER_BINARY");
        let base = artifact(&base_path);
        let target = artifact(&target_path);
        let reader = if cfg!(windows) {
            "reader.exe"
        } else {
            "reader"
        };
        let installed = workspace.join(reader);
        let data = workspace.join("data");
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
        let programs=ids.iter().map(|id|{let check=matches!(*id,"check_base"|"check_data"|"check_target"|"exercise");serde_json::json!({"id":id,"program":if check{&installed}else{&installer},"args":if check{vec![id.to_string(),data.to_str().unwrap().into()]}else{vec![id.to_string(),workspace.to_str().unwrap().into(),base_path.to_str().unwrap().into(),target_path.to_str().unwrap().into(),reader.into()]},"environment":{"kind":"replace","entries":[]}})}).collect::<Vec<_>>();
        let binding = v2::BindingId::from_bytes([1; 16]);
        let mut random = [0; 32];
        theseus_host::random_bytes(&mut random).unwrap();
        let epoch = v2::Digest::from_bytes(random);
        let os = if cfg!(windows) {
            v2::Os::Windows
        } else if cfg!(target_os = "macos") {
            v2::Os::Macos
        } else {
            v2::Os::Linux
        };
        let mut config_data = serde_json::json!({"schema":2,"profile":"native_catalog_v1","capabilities":v2::NativeCapabilities::NAMES,"provider_epoch":epoch.to_hex(),"artifacts":{"base":base,"target":target},"ledger_root":ledger_path,"workspace_root":workspace,"binding":binding.to_hex(),"manifest_digest":"00".repeat(32),"workspace_marker":"00".repeat(16),"os":if cfg!(windows){"windows"}else if cfg!(target_os="macos"){"macos"}else{"linux"},"programs":programs});
        let digest = theseus_native_helper::config_manifest_digest(
            &serde_json::to_vec(&config_data).unwrap(),
        )
        .unwrap();
        let marker = bootstrap_native(
            &ledger,
            &workspace_dir,
            BootstrapRequest {
                binding,
                manifest_digest: digest,
                os,
            },
            epoch,
        )
        .unwrap()
        .workspace_marker();
        config_data["manifest_digest"] = digest.to_hex().into();
        config_data["workspace_marker"] = marker.to_hex().into();
        let config = root.path().join("config.json");
        std::fs::write(&config, serde_json::to_vec(&config_data).unwrap()).unwrap();
        let provider_root = root.path().join("provider");
        std::fs::create_dir(&provider_root).unwrap();
        let template = root.path().join("provider-template.json");
        std::fs::write(&template,serde_json::to_vec(&serde_json::json!({"provider":"inert_provider","invocation_id":"placeholder","invocation_digest":"00".repeat(32),"binding":binding.to_hex(),"manifest_digest":digest.to_hex(),"workspace_marker":marker.to_hex(),"reset_epoch":epoch.to_hex(),"receipt":"placeholder"})).unwrap()).unwrap();
        Self {
            root,
            workspace,
            config,
            digest,
            binding,
            marker,
            epoch,
            base,
            target,
            helper,
            installer,
            provider_root,
            template,
        }
    }
    pub fn launch(&self) -> LaunchAuthority {
        let mut helper = NativeCommand::new(&self.helper);
        helper.args(["serve", "--config", self.config.to_str().unwrap()]);
        helper.environment = ChildEnvironment::Replace(vec![]);
        let mut provider = NativeCommand::new(&self.installer);
        provider.args([
            "provider",
            self.provider_root.to_str().unwrap(),
            self.template.to_str().unwrap(),
        ]);
        provider.environment = ChildEnvironment::Replace(vec![]);
        let os = if cfg!(windows) {
            v2::Os::Windows
        } else if cfg!(target_os = "macos") {
            v2::Os::Macos
        } else {
            v2::Os::Linux
        };
        LaunchAuthority::fixture(
            helper,
            StoreBinding {
                binding: self.binding,
                manifest_digest: self.digest,
                workspace_marker: self.marker,
                os,
            },
            self.epoch,
        )
        .with_fixture_provider("inert_provider".into(), provider)
        .unwrap()
    }
    pub fn recipe(&self, kind: ScenarioKind) -> ValidatedNativeRecipe {
        let status = if cfg!(windows) {
            NativeStatus::WindowsExit { code: 0 }
        } else {
            NativeStatus::PosixExit { code: 0 }
        };
        let command = |id: &str, program, literal: Option<&str>| CatalogCommand {
            id: id.into(),
            program,
            expected_status: status.clone(),
            stdout_literal: literal.map(str::to_owned),
        };
        let mut steps = BTreeMap::from([
            (
                StepName::InstallTarget,
                command("install_target", CatalogProgram::InstallTarget, None),
            ),
            (
                StepName::Configure,
                command("configure", CatalogProgram::Configure, None),
            ),
            (
                StepName::Exercise,
                command("exercise", CatalogProgram::Exercise, Some("target\n")),
            ),
            (
                StepName::Uninstall,
                command("uninstall", CatalogProgram::Uninstall, None),
            ),
        ]);
        let mut checks = vec![
            NativeCheck {
                id: "target_version".into(),
                checkpoint: Checkpoint::TargetInstalled,
                command: command(
                    "target_version",
                    CatalogProgram::CheckTarget,
                    Some("target\n"),
                ),
            },
            NativeCheck {
                id: "configured_data".into(),
                checkpoint: Checkpoint::Configured,
                command: command("configured_data", CatalogProgram::CheckData, Some("data\n")),
            },
        ];
        if kind == ScenarioKind::Upgrade {
            steps.insert(
                StepName::InstallBase,
                command("install_base", CatalogProgram::InstallBase, None),
            );
            steps.insert(
                StepName::Upgrade,
                command("upgrade", CatalogProgram::InstallTarget, None),
            );
            checks.push(NativeCheck {
                id: "base_version".into(),
                checkpoint: Checkpoint::BaseInstalled,
                command: command("base_version", CatalogProgram::CheckBase, Some("base\n")),
            });
            checks.push(NativeCheck {
                id: "upgraded_data".into(),
                checkpoint: Checkpoint::TargetInstalled,
                command: command("upgraded_data", CatalogProgram::CheckData, Some("data\n")),
            });
        }
        if kind == ScenarioKind::Reinstall {
            steps.insert(
                StepName::Reinstall,
                command("reinstall", CatalogProgram::InstallTarget, None),
            );
            checks.push(NativeCheck {
                id: "reinstalled_version".into(),
                checkpoint: Checkpoint::Reinstalled,
                command: command(
                    "reinstalled_version",
                    CatalogProgram::CheckTarget,
                    Some("target\n"),
                ),
            });
            checks.push(NativeCheck {
                id: "reinstalled_data".into(),
                checkpoint: Checkpoint::Reinstalled,
                command: command(
                    "reinstalled_data",
                    CatalogProgram::CheckData,
                    Some("data\n"),
                ),
            });
        }
        let recipe = NativeRecipe {
            kind: "theseus_native_recipe".into(),
            schema: 1,
            os: if cfg!(windows) {
                NativeOs::Windows
            } else if cfg!(target_os = "macos") {
                NativeOs::Macos
            } else {
                NativeOs::Linux
            },
            manifest_digest: self.digest.to_hex(),
            leaves: NativeLeaves {
                reader: if cfg!(windows) {
                    "reader.exe"
                } else {
                    "reader"
                }
                .into(),
                app: "app".into(),
                data: "data".into(),
            },
            base: self.base.clone(),
            target: self.target.clone(),
            scenarios: vec![kind],
            steps,
            checks,
            limits: NativeRunLimits::default(),
        };
        ValidatedNativeRecipe::decode(
            &serde_json::to_vec(&recipe).unwrap(),
            self.root.path().join("recipe.json"),
        )
        .unwrap()
    }
}
