//! Compiled SourceOwned desktop plus an explicitly supplied real private manager.
//! One provider/helper/ordinary sequence; no native activation or manager fallback.
#![allow(dead_code)]
#[path = "native_service_setup.rs"]
mod service_setup;
use super::setup::{Operator, pin, write};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::path::{Path, PathBuf};
use theseus::{
    model::{RunOutcome, ScenarioKind, StepName},
    native::{integration::model::*, storage},
};
use theseus_host::fs::HostDir;
use theseus_native_protocol::v2;

pub struct CombinedFixture {
    pub service: service_setup::IntegrationFixture,
    operator: Option<Operator>,
    sockets: Option<tempfile::TempDir>,
    launch: PathBuf,
}
impl CombinedFixture {
    pub fn root(&self) -> &Path {
        self.service.root.path()
    }
    pub fn launch(&self) -> &Path {
        &self.launch
    }
    pub fn new(scenario: ScenarioKind) -> Result<Self> {
        Self::from_service(
            service_setup::IntegrationFixture::supplied_manager(
                ManagerKind::SystemdUser,
                service_setup::Fault::None,
            )?,
            scenario,
        )
    }
    pub fn private_model(scenario: ScenarioKind) -> Result<Self> {
        let receipt = PathBuf::from(
            std::env::var_os("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT").context(
                "explicit private-model receipt required; cannot satisfy real manager gate",
            )?,
        );
        Self::from_service(
            service_setup::IntegrationFixture::model_manager(&receipt, service_setup::Fault::None)?,
            scenario,
        )
    }
    fn from_service(
        mut service: service_setup::IntegrationFixture,
        scenario: ScenarioKind,
    ) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let mut recipe = service.scenario_recipe(scenario)?.recipe;
        let root = service.root.path();
        let mut graph: Value = serde_json::from_slice(&std::fs::read(&service.graph_path)?)?;
        let config_path = PathBuf::from(graph["config"]["path"].as_str().context("config pin")?);
        let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path)?)?;
        let artifacts = root.join("artifacts");
        std::fs::set_permissions(&artifacts, std::fs::Permissions::from_mode(0o700))?;
        let copy = |variable: &str, name: &str| -> Result<PathBuf> {
            let source = PathBuf::from(
                std::env::var_os(variable)
                    .with_context(|| format!("required explicit {variable}"))?,
            );
            ensure!(
                source.is_absolute() && source.is_file(),
                "absolute pinned compiled artifact"
            );
            let destination = artifacts.join(name);
            std::fs::copy(source, &destination)?;
            std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o500))?;
            Ok(destination)
        };
        let helper = copy("THESEUS_NATIVE_DESKTOP_HELPER_BINARY", "combined-helper")?;
        let broker = copy("THESEUS_NATIVE_DESKTOP_BROKER_BINARY", "combined-broker")?;
        std::fs::set_permissions(&artifacts, std::fs::Permissions::from_mode(0o500))?;
        for program in config["programs"].as_array_mut().context("core catalog")? {
            if ["install_base", "install_target", "configure", "uninstall"]
                .contains(&program["id"].as_str().unwrap_or(""))
            {
                program["args"]
                    .as_array_mut()
                    .context("fixed SUT args")?
                    .push("--theseus-combined-entry-only".into());
            }
        }
        let mut core_config = config.clone();
        core_config["schema"] = 2.into();
        core_config["profile"] = "native_catalog_v1".into();
        core_config["capabilities"] = json!(v2::NativeCapabilities::NAMES);
        core_config["manifest_digest"] = "00".repeat(32).into();
        for field in [
            "integration_manifest",
            "runtime_context",
            "core_manifest_digest",
            "integration_manifest_digest",
        ] {
            core_config.as_object_mut().unwrap().remove(field);
        }
        let core =
            theseus_native_helper::config_manifest_digest(&serde_json::to_vec(&core_config)?)?;
        let mut manifest: v2::IntegrationManifest =
            serde_json::from_value(config["integration_manifest"].clone())?;
        manifest.core_manifest_digest = core.to_hex();
        let workspace = PathBuf::from(config["workspace_root"].as_str().context("workspace")?);
        let entries = root.join("entries");
        for directory in [
            &entries,
            &root.join("broker-ledger"),
            &root.join("broker-workspace"),
        ] {
            HostDir::create_private(directory)?;
        }
        let sockets = tempfile::Builder::new()
            .prefix("th-combined-")
            .tempdir_in("/tmp")?;
        std::fs::set_permissions(sockets.path(), std::fs::Permissions::from_mode(0o700))?;
        let ready = sockets.path().join("ready.sock");
        let close = sockets.path().join("close.sock");
        let data_sha = format!("{:x}", Sha256::digest(b"theseus fixture data\n"));
        let expectation = v2::ReadinessDefinitionExpectation {
            role: "configured".into(),
            artifact: "target".into(),
            artifact_length: recipe.catalog.target.length,
            artifact_sha256: recipe.catalog.target.sha256.clone(),
            version_literal: "target\n".into(),
            data_sha256: data_sha.clone(),
            configuration_digest: data_sha,
        };
        let executable = v2::DesktopExecutable {
            root: workspace.display().to_string(),
            relative_path: "reader".into(),
            length: expectation.artifact_length,
            sha256: expectation.artifact_sha256.clone(),
            arguments: vec![
                workspace.join("reader").display().to_string(),
                "desktop".into(),
                ready.display().to_string(),
                close.display().to_string(),
                workspace.join("data").display().to_string(),
                "original-source-combined".into(),
            ],
            environment: vec![],
            working_directory: workspace.display().to_string(),
        };
        manifest.desktop_executable = Some(executable);
        manifest.desktop_readiness = Some(expectation.clone());
        let bytes = super::setup::installer::entry_bytes(&manifest)?;
        let entry_digest = format!("{:x}", Sha256::digest(&bytes));
        let slot = v2::ResourceSlot::new("desktop")?;
        manifest.desktop = Some(v2::broker::DesktopManifest {
            schema: 1,
            namespace_policy: "exclusive_quiescent".into(),
            core_manifest_digest: core.to_hex(),
            slot: slot.clone(),
            entry_root: entries.display().to_string(),
            entry: "theseus-source.desktop".into(),
            entry_digest: entry_digest.clone(),
            executable_digest: expectation.artifact_sha256.clone(),
            allowed_modes: vec![v2::broker::DesktopMode::OwnedExecutable],
            ready_endpoint: v2::LocalEndpoint::UnixPath {
                path: ready.display().to_string(),
            },
            close_endpoint: v2::LocalEndpoint::UnixPath {
                path: close.display().to_string(),
            },
            expectation_digest: expectation.digest()?.to_hex(),
        });
        manifest.desktop_capabilities = v2::IntegrationCapabilities::DESKTOP_NAMES
            .into_iter()
            .map(str::to_owned)
            .collect();
        manifest.artifact_slots.push(v2::IntegrationArtifactSlot {
            slot,
            relative_path: "theseus-source.desktop".into(),
            length: bytes.len() as u64,
            sha256: entry_digest,
        });
        for (role, artifact) in [
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
            let mut d = v2::DesktopDefinition {
                key: v2::DesktopDefinitionKey { role, artifact },
                entry_length: bytes.len() as u64,
                desktop: manifest.desktop.clone().unwrap(),
                executable: manifest.desktop_executable.clone().unwrap(),
                readiness: expectation.clone(),
                native: None,
            };
            let source = if artifact == v2::DesktopArtifactRole::Base {
                &recipe.catalog.base
            } else {
                &recipe.catalog.target
            };
            d.executable.length = source.length;
            d.executable.sha256 = source.sha256.clone();
            d.readiness.role = role.as_str().into();
            d.readiness.artifact = artifact.as_str().into();
            d.readiness.artifact_length = source.length;
            d.readiness.artifact_sha256 = source.sha256.clone();
            d.readiness.version_literal = if artifact == v2::DesktopArtifactRole::Base {
                "base\n"
            } else {
                "target\n"
            }
            .into();
            d.desktop.executable_digest = source.sha256.clone();
            d.desktop.expectation_digest = d.readiness.digest()?.to_hex();
            manifest.desktop_definitions.push(d);
        }
        manifest.validate()?;
        let integration = manifest.digest()?;
        let full = v2::integration_manifest_digest(core, integration);
        let mut context: v2::IntegrationRuntimeContext =
            serde_json::from_value(config["runtime_context"].clone())?;
        context.manifest_digest = full.to_hex();
        context.integration_manifest_digest = integration.to_hex();
        context.ownership_id = "03".repeat(16);
        let theseus_host::principal::CurrentPrincipal::Posix {
            effective_uid,
            effective_gid,
        } = theseus_host::principal::current_principal()?
        else {
            anyhow::bail!("source Linux principal")
        };
        let header = v2::broker::IntegrationBrokerHeader {
            linux_gio_qualification: None,
            macos_security_session_id: None,
            profile: "native_integration_broker_v1".into(),
            binding: context.binding.clone(),
            manifest_digest: full.to_hex(),
            workspace_marker: context.workspace_marker.clone(),
            os: "linux".into(),
            target_id: "source-combined-target".into(),
            ownership_id: context.ownership_id.clone(),
            provider_epoch: context.provider_epoch.clone(),
            session_id: "source-combined-owned".into(),
            session_epoch: context.provider_epoch.clone(),
            broker_endpoint: v2::LocalEndpoint::UnixPath {
                path: sockets.path().join("broker.sock").display().to_string(),
            },
            broker_principal: v2::broker::BrokerPrincipal::Posix {
                effective_uid,
                effective_gid,
            },
            helper_principal: v2::broker::BrokerPrincipal::Posix {
                effective_uid,
                effective_gid,
            },
            display: v2::broker::SuppliedDisplay::SourceOwnedHeadless,
            allowed_modes: vec![v2::broker::DesktopMode::OwnedExecutable],
        };
        context.desktop_broker_header = Some(header.clone());
        manifest.validate_context(&context, v2::Os::Linux)?;
        config["core_manifest_digest"] = core.to_hex().into();
        config["integration_manifest_digest"] = integration.to_hex().into();
        config["manifest_digest"] = full.to_hex().into();
        config["integration_manifest"] = serde_json::to_value(&manifest)?;
        config["runtime_context"] = serde_json::to_value(&context)?;
        write(&config_path, &config)?;
        let broker_path = root.join("broker.json");
        write(
            &broker_path,
            &json!({"kind":"theseus_native_desktop_broker","schema":1,"profile":"native_integration_broker_v1","ledger_root":root.join("broker-ledger"),"workspace_root":root.join("broker-workspace"),"header":header,"manifest":manifest,"source_parent_eof":true}),
        )?;
        let template = PathBuf::from(
            graph["provider_template"]["path"]
                .as_str()
                .context("provider template")?,
        );
        let mut receipt: theseus::native::model::ProviderReceipt =
            serde_json::from_slice(&std::fs::read(&template)?)?;
        receipt.manifest_digest = full.to_hex();
        write(&template, &receipt)?;
        recipe.integration_manifest_digest = integration.to_hex();
        recipe.catalog.manifest_digest = core.to_hex();
        recipe.integrations.desktop = Some(DesktopRequirement {
            resource: ResourceSlot::new("desktop")?,
            modes: vec![DesktopMode::OwnedExecutable],
            readiness: ReadinessRequirement {
                kind: ReadyKind::LocalChallenge,
                protocol: ReadyProtocol::TheseusFixtureReadyV1,
                deadline_ms: 30000,
            },
            lifetime: LifetimeRequirement::OwnedProcessTreeSettled,
        });
        write(&service.recipe_path, &recipe)?;
        service.recipe = theseus::native::integration::ValidatedIntegrationRecipe::decode(
            &std::fs::read(&service.recipe_path)?,
            service.recipe_path.clone(),
        )?;
        let launch = PathBuf::from(graph["launch"]["path"].as_str().context("launch")?);
        write(
            &launch,
            &json!({"fixture":"source-owned-combined","manifest_digest":full.to_hex()}),
        )?;
        graph["launch"] = pin(&launch)?;
        graph["helper"] = pin(&helper)?;
        graph["config"] = pin(&config_path)?;
        graph["provider_template"] = pin(&template)?;
        graph["manifest_digest"] = full.to_hex().into();
        graph["core_manifest_digest"] = core.to_hex().into();
        graph["integration_manifest_digest"] = integration.to_hex().into();
        graph["desktop_authority"] = serde_json::to_value(header.authority())?;
        graph["source_mode"] = "source_owned_combined".into();
        graph["broker"] = pin(&broker)?;
        graph["broker_config"] = pin(&broker_path)?;
        write(&service.graph_path, &graph)?;
        let provider_root = PathBuf::from(
            graph["provider_root"]
                .as_str()
                .context("one provider root")?,
        );
        let operator = Operator::start(provider_root, broker, broker_path);
        Ok(Self {
            service,
            operator: Some(operator),
            sockets: Some(sockets),
            launch,
        })
    }
    pub fn finish(mut self) -> Result<()> {
        if let Some(operator) = &mut self.operator {
            operator.finish()?;
        }
        self.operator.take();
        Ok(())
    }
}
impl Drop for CombinedFixture {
    fn drop(&mut self) {
        if let Some(operator) = &mut self.operator
            && let Err(error) = operator.finish()
        {
            eprintln!("combined original broker cleanup unresolved: {error:#}");
        }
        self.operator.take();
        if let Some(sockets) = self.sockets.take() {
            eprintln!("retained combined endpoints: {}", sockets.keep().display());
        }
    }
}

pub fn assert_shared_original(
    report: &NativeIntegrationReport,
    directory: &Path,
    scenario: ScenarioKind,
) -> Result<()> {
    ensure!(
        report.outcome == RunOutcome::Passed
            && report.catalog.scenarios.len() == 1
            && report.integrations.len() == 1,
        "one complete combined ordinary scenario"
    );
    let integration = &report.integrations[0];
    ensure!(
        integration.services.len() == 1 && integration.desktops.len() == 1,
        "both declared resources observed"
    );
    let theseus::engine::domain::StoredJournal::NativeIntegration(journal) =
        storage::load_journal(directory)?
    else {
        anyhow::bail!("same combined journal")
    };
    validate_pair(report, &journal)?;
    ensure!(
        journal.ownership.len() == 1 && journal.broker_bindings.len() == 1,
        "one original ownership/helper/broker lease"
    );
    let lease = journal.broker_bindings[0].helper_lease.as_str();
    ensure!(
        journal.operations.iter().all(|o| o.lease == lease)
            && journal.integration_actions.iter().all(|a| a.lease == lease),
        "both resources share exact original helper lease"
    );
    let steps = report.catalog.scenarios[0]
        .stages
        .iter()
        .filter_map(|s| s.step)
        .collect::<Vec<_>>();
    let expected = match scenario {
        ScenarioKind::Clean => vec![
            StepName::InstallTarget,
            StepName::Configure,
            StepName::Exercise,
            StepName::Uninstall,
        ],
        ScenarioKind::Upgrade => vec![
            StepName::InstallBase,
            StepName::Configure,
            StepName::Upgrade,
            StepName::Exercise,
            StepName::Uninstall,
        ],
        ScenarioKind::Reinstall => vec![
            StepName::InstallTarget,
            StepName::Configure,
            StepName::Exercise,
            StepName::Reinstall,
            StepName::Exercise,
            StepName::Uninstall,
        ],
        _ => anyhow::bail!("ordinary combined scenario"),
    };
    ensure!(steps == expected, "one shared ordinary SUT sequence");
    ensure!(
        integration
            .checks
            .iter()
            .all(|c| c.outcome == theseus::model::CheckOutcome::Passed),
        "independent required service and desktop facts"
    );
    let prepares = journal
        .provider_calls
        .iter()
        .filter(|call| call.invocation.action == theseus::native::model::ProviderAction::Prepare)
        .count();
    ensure!(
        prepares == 1,
        "one provider Prepare, not separate desktop/service providers"
    );
    Ok(())
}
