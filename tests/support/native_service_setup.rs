//! Supplied-manager acceptance fixture. Missing qualification fails; no model fallback.
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
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagerReceipt {
    kind: String,
    schema: u32,
    manager: ManagerKind,
    authority: v2::ServiceAuthority,
    manager_unique_name: String,
    namespace_receipt: String,
}
impl ManagerReceipt {
    fn authority_version(&self) -> &str {
        match &self.authority {
            v2::ServiceAuthority::Systemd { qualification, .. } => &qualification.version,
            _ => "unsupported_fixture_platform",
        }
    }
}
#[derive(Clone, Copy)]
pub enum Fault {
    None,
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
            Self::None => "none",
            Self::MissingService => "missing_service",
            Self::LeakedService => "leaked_service",
            Self::WrongVersion => "wrong_version",
            Self::WrongData => "wrong_data",
            Self::WrongNonce => "wrong_nonce",
            Self::WrongPeer => "wrong_peer",
        }
    }
}
pub struct EvidenceDirectory(Option<tempfile::TempDir>);
impl EvidenceDirectory {
    fn new(directory: tempfile::TempDir) -> Self {
        Self(Some(directory))
    }
    pub fn path(&self) -> &Path {
        self.0.as_ref().expect("retained fixture directory").path()
    }
}
impl Drop for EvidenceDirectory {
    fn drop(&mut self) {
        if let Some(directory) = self.0.take() {
            eprintln!(
                "retained service fixture evidence: {}",
                directory.keep().display()
            );
        }
    }
}
pub struct IntegrationFixture {
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
            cfg!(target_os = "linux") && manager == ManagerKind::SystemdUser,
            "first supplied fixture requires systemd user manager"
        );
        let receipt_path=PathBuf::from(std::env::var_os("THESEUS_NATIVE_SYSTEMD_RECEIPT").ok_or_else(||anyhow::anyhow!("real private systemd fixture qualification is required; no model or skip fallback"))?);
        let receipt_bytes = std::fs::read(receipt_path)?;
        let receipt: ManagerReceipt = serde_json::from_slice(&receipt_bytes)?;
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
        let root = EvidenceDirectory::new(tempfile::tempdir()?);
        let ready_root = EvidenceDirectory::new(
            tempfile::Builder::new()
                .prefix("th-ready-")
                .tempdir_in("/tmp")?,
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for directory in [root.path(), ready_root.path()] {
                std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
                ensure!(
                    std::fs::metadata(directory)?.permissions().mode() & 0o077 == 0,
                    "fixture directory must be private before any helper effect"
                );
            }
        }
        let workspace = root.path().join("workspace");
        let ledger = root.path().join("ledger");
        let provider_root = root.path().join("provider");
        for path in [&workspace, &ledger, &provider_root] {
            HostDir::create_private(path)?;
        }
        let artifacts = root.path().join("artifacts");
        HostDir::create_private(&artifacts)?;
        let copy = |variable: &str, name: &str| -> Result<PathBuf> {
            let source = binary(variable)?;
            let destination = artifacts.join(name);
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
        let reader = workspace.join("reader");
        let data = workspace.join("data");
        let socket = ready_root.path().join("ready");
        let mut random = [0; 32];
        theseus_host::random_bytes(&mut random)?;
        let epoch = v2::Digest::from_bytes(random);
        let unit = format!("theseus-{}.service", &epoch.to_hex()[..20]);
        let v2::ServiceAuthority::Systemd {
            unit: unit_name, ..
        } = &mut receipt.authority
        else {
            anyhow::bail!("systemd fixture authority required")
        };
        *unit_name = unit;
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
            let check=matches!(*id,"check_base"|"check_data"|"check_target"|"exercise");
            serde_json::json!({"id":id,"program":if check{&reader}else{&sut},"args":if check{vec![id.to_string(),data.display().to_string()]}else{vec![id.to_string(),config_path.display().to_string(),base_path.display().to_string(),target_path.display().to_string(),fault.name().into()]},"environment":{"kind":"replace","entries":[]}})
        }).collect::<Vec<_>>();
        let mut config = serde_json::json!({"schema":2,"profile":"native_catalog_v1","capabilities":v2::NativeCapabilities::NAMES,"provider_epoch":epoch.to_hex(),"artifacts":{"base":base,"target":target},"ledger_root":ledger,"workspace_root":workspace,"binding":"01".repeat(16),"manifest_digest":"00".repeat(32),"workspace_marker":"02".repeat(16),"os":"linux","programs":programs});
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
            d.definition_sha256 = format!("{:x}", Sha256::digest(definition_bytes(&d)));
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
        let v2::ServiceAuthority::Systemd {
            expected_manager_uid,
            ..
        } = &receipt.authority
        else {
            anyhow::bail!("systemd fixture authority required")
        };
        #[cfg(unix)]
        let gid = rustix::process::getegid().as_raw();
        #[cfg(not(unix))]
        let gid = 0;
        let manifest = v2::IntegrationManifest {
            schema: 1,
            core_manifest_digest: core.to_hex(),
            service_authority: receipt.authority.clone(),
            definitions,
            artifact_slots: vec![],
            readiness_protocol: "theseus_fixture_ready_v1".into(),
            desktop_capabilities: vec![],
            readiness: Some(v2::IntegrationReadiness {
                peer_sid: None,
                cooperative_instance: None,
                endpoint: v2::LocalEndpoint::UnixPath {
                    path: socket.display().to_string(),
                },
                root: ready_root.path().display().to_string(),
                peer_uid: Some(if matches!(fault, Fault::WrongPeer) {
                    expected_manager_uid.checked_add(1).unwrap()
                } else {
                    *expected_manager_uid
                }),
                peer_gid: Some(gid),
                expectations,
            }),
        };
        let integration = manifest.digest()?;
        let full = v2::integration_manifest_digest(core, integration);
        let context = v2::IntegrationRuntimeContext {
            manager_platform: v2::ManagerPlatform::Systemd,
            installation_instance_digest: None,
            kind: "theseus_native_integration_context".into(),
            schema: 1,
            ownership_id: "provider-reservation-pending".into(),
            binding: "01".repeat(16),
            manifest_digest: full.to_hex(),
            workspace_marker: "02".repeat(16),
            provider_epoch: epoch.to_hex(),
            integration_manifest_digest: integration.to_hex(),
            manager_unique_name: receipt.manager_unique_name,
            namespace_receipt: receipt.namespace_receipt,
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
            &serde_json::json!({"kind":"theseus_native_integration_fixture_graph","schema":1,"launch":pin(&launch_path)?,"controller":pin(&controller_path)?,"base":pin(&base_path)?,"target":pin(&target_path)?,"helper":pin(&helper)?,"config":pin(&config_path)?,"provider":pin(&sut)?,"provider_template":pin(&template_path)?,"provider_root":provider_root,"binding":context.binding,"manifest_digest":full.to_hex(),"workspace_marker":context.workspace_marker,"provider_epoch":epoch.to_hex(),"core_manifest_digest":core.to_hex(),"integration_manifest_digest":integration.to_hex(),"service_authority":receipt.authority}),
        )?;
        let output = root.path().join(".theseus-runs");
        Ok(Self {
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
