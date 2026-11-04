//! Explicit model provider for an already supplied private manager context.
//! This reserves fixture ownership; it makes no claim to provision/reset a VM.
use anyhow::{Result, ensure};
use std::{io::Write, path::Path};
use theseus::native::model::{ProviderAction, ProviderInvocation, ProviderReceipt};
use theseus_host::fs::{HostDir, RootPolicy};
use theseus_native_helper_store::{BootstrapRequest, bootstrap_integration};
use theseus_native_protocol::v2;

pub fn run(args: &[String]) -> Result<()> {
    ensure!(
        args.len() == 5,
        "expected fixture provider root/template/config/mode/invocation"
    );
    let root = Path::new(&args[0]);
    let _retained_root = HostDir::open(root, RootPolicy::AllComponentsNoFollow)?;
    ensure!(
        root.is_absolute(),
        "explicit private fixture provider root required"
    );
    let invocation: ProviderInvocation = serde_json::from_str(&args[4])?;
    invocation.validate()?;
    for identity in [&invocation.ownership_id, &invocation.invocation_id] {
        ensure!(
            !identity.is_empty()
                && identity
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
            "invalid fixture identity"
        );
    }
    ensure!(
        invocation.provider == "integration_fixture_provider",
        "wrong fixture provider"
    );
    let saved = root.join(format!("{}.json", invocation.invocation_id));
    if args[3] == "query" {
        match std::fs::read(&saved) {
            Ok(bytes) => {
                let value: serde_json::Value = serde_json::from_slice(&bytes)?;
                ensure!(
                    value["invocation"] == serde_json::to_value(&invocation)?,
                    "query must retain original invocation"
                );
                print!("{}", serde_json::to_string(&value["receipt"])?);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => print!("null"),
            Err(error) => return Err(error.into()),
        }
        return Ok(());
    }
    ensure!(args[3] == "invoke", "unknown fixture provider mode");
    let reservation = root.join("owner");
    if invocation.action == ProviderAction::Prepare {
        let mut owner = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&reservation)?;
        owner.write_all(invocation.ownership_id.as_bytes())?;
        owner.sync_all()?;
    } else {
        ensure!(
            std::fs::read(&reservation)? == invocation.ownership_id.as_bytes(),
            "original fixture owner missing"
        );
    }
    let mut receipt: ProviderReceipt = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    receipt.provider = invocation.provider.clone();
    receipt.invocation_id = invocation.invocation_id.clone();
    receipt.invocation_digest = invocation.digest()?;
    if invocation.action == ProviderAction::Prepare {
        let config_path = Path::new(&args[2]);
        let mut config: serde_json::Value = serde_json::from_slice(&std::fs::read(config_path)?)?;
        let mut context: v2::IntegrationRuntimeContext =
            serde_json::from_value(config["runtime_context"].clone())?;
        context.ownership_id = invocation.ownership_id.clone();
        context.validate()?;
        let field = |name: &str| -> Result<&str> {
            config[name]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("fixture config missing {name}"))
        };
        let ledger = HostDir::open(
            Path::new(field("ledger_root")?),
            RootPolicy::AllComponentsNoFollow,
        )?;
        let workspace = HostDir::open(
            Path::new(field("workspace_root")?),
            RootPolicy::AllComponentsNoFollow,
        )?;
        bootstrap_integration(
            &ledger,
            &workspace,
            BootstrapRequest {
                binding: v2::BindingId::parse(field("binding")?)?,
                manifest_digest: v2::Digest::parse(field("manifest_digest")?)?,
                os: if cfg!(windows) {
                    v2::Os::Windows
                } else if cfg!(target_os = "macos") {
                    v2::Os::Macos
                } else {
                    v2::Os::Linux
                },
            },
            &context,
            v2::Digest::parse(field("core_manifest_digest")?)?,
            v2::Digest::parse(field("integration_manifest_digest")?)?,
            v2::artifact_commitment(
                config["artifacts"]["base"]["length"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("missing base length"))?,
                v2::Digest::parse(
                    config["artifacts"]["base"]["sha256"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("missing base digest"))?,
                )?,
                config["artifacts"]["target"]["length"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("missing target length"))?,
                v2::Digest::parse(
                    config["artifacts"]["target"]["sha256"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("missing target digest"))?,
                )?,
            )?,
        )?;
        receipt.receipt = serde_json::to_string(&context)?;
        config["runtime_context"] = serde_json::to_value(context)?;
        let temporary =
            config_path.with_extension(format!("{}.prepared", invocation.invocation_id));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(&config)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, config_path)?;
    } else {
        let mut epoch = [0; 32];
        theseus_host::random_bytes(&mut epoch)?;
        receipt.reset_epoch = v2::Digest::from_bytes(epoch).to_hex();
        receipt.receipt = format!(
            "model-provider-reservation-released-{}",
            invocation.invocation_id
        );
    }
    receipt.validate()?;
    let record = serde_json::json!({"invocation":invocation,"receipt":receipt});
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&saved)?;
    file.write_all(&serde_json::to_vec(&record)?)?;
    file.sync_all()?;
    if invocation.action != ProviderAction::Prepare {
        std::fs::remove_file(reservation)?;
    }
    print!("{}", serde_json::to_string(&receipt)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use theseus::native::model::{NativeCatalogAuthority, NativeOwnership, NativeResource};
    struct Fixture {
        root: tempfile::TempDir,
        args: Vec<String>,
        invocation: ProviderInvocation,
    }
    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let ledger = root.path().join("ledger");
        let workspace = root.path().join("workspace");
        let provider = root.path().join("provider");
        for path in [&ledger, &workspace, &provider] {
            HostDir::create_private(path).unwrap();
        }
        let core = v2::Digest::from_bytes([3; 32]);
        let integration = v2::Digest::from_bytes([4; 32]);
        let full = v2::integration_manifest_digest(core, integration);
        let context = v2::IntegrationRuntimeContext {
            manager_platform: v2::ManagerPlatform::Systemd,
            installation_instance_digest: None,
            kind: "theseus_native_integration_context".into(),
            schema: 1,
            ownership_id: "not-yet-reserved".into(),
            binding: "01".repeat(16),
            manifest_digest: full.to_hex(),
            workspace_marker: "02".repeat(16),
            provider_epoch: "05".repeat(32),
            integration_manifest_digest: integration.to_hex(),
            manager_unique_name: ":1.42".into(),
            namespace_receipt: "explicit-model-context".into(),
        };
        let config = root.path().join("config.json");
        std::fs::write(&config,serde_json::to_vec(&serde_json::json!({"ledger_root":ledger,"workspace_root":workspace,"binding":context.binding,"manifest_digest":context.manifest_digest,"core_manifest_digest":core.to_hex(),"integration_manifest_digest":integration.to_hex(),"runtime_context":context,"artifacts":{"base":{"length":1,"sha256":"07".repeat(32)},"target":{"length":2,"sha256":"08".repeat(32)}}})).unwrap()).unwrap();
        let template = root.path().join("template.json");
        let receipt = ProviderReceipt {
            provider: "integration_fixture_provider".into(),
            invocation_id: "placeholder".into(),
            invocation_digest: "00".repeat(32),
            binding: context.binding,
            manifest_digest: context.manifest_digest,
            workspace_marker: context.workspace_marker,
            reset_epoch: context.provider_epoch,
            receipt: "placeholder".into(),
        };
        std::fs::write(&template, serde_json::to_vec(&receipt).unwrap()).unwrap();
        let invocation = ProviderInvocation {
            ownership_id: "owner-original".into(),
            provider: "integration_fixture_provider".into(),
            invocation_id: "prepare-original".into(),
            action: ProviderAction::Prepare,
            target_id: "model-target".into(),
            baseline_label: "model-only".into(),
            launch_fingerprint: "06".repeat(32),
            prior_resource: NativeResource {
                scenario_id: "clean".into(),
                ownership_id: "owner-original".into(),
                ownership: NativeOwnership::Preparing {
                    provider: "integration_fixture_provider".into(),
                    invocation_id: "prepare-original".into(),
                    receipt: None,
                },
                catalog_authority: NativeCatalogAuthority::Valid,
            },
        };
        let args = vec![
            provider.display().to_string(),
            template.display().to_string(),
            config.display().to_string(),
            "invoke".into(),
            serde_json::to_string(&invocation).unwrap(),
        ];
        Fixture {
            root,
            args,
            invocation,
        }
    }
    #[test]
    fn prepare_binds_original_owner_in_config_store_and_query_receipt() {
        let f = fixture();
        run(&f.args).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(
            &std::fs::read(f.root.path().join("provider/prepare-original.json")).unwrap(),
        )
        .unwrap();
        let receipt: ProviderReceipt = serde_json::from_value(saved["receipt"].clone()).unwrap();
        let context: v2::IntegrationRuntimeContext =
            serde_json::from_str(&receipt.receipt).unwrap();
        assert_eq!(context.ownership_id, f.invocation.ownership_id);
        assert_eq!(receipt.invocation_digest, f.invocation.digest().unwrap());
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&f.args[2]).unwrap()).unwrap();
        assert_eq!(
            config["runtime_context"],
            serde_json::to_value(context).unwrap()
        );
        assert!(f.root.path().join("ledger/ledger.bin").is_file());
        assert!(
            f.root
                .path()
                .join("workspace/.theseus-workspace-marker")
                .is_file()
        );
        let mut query = f.args.clone();
        query[3] = "query".into();
        run(&query).unwrap();
        let mut changed = f.invocation;
        changed.baseline_label = "different".into();
        query[4] = serde_json::to_string(&changed).unwrap();
        assert!(run(&query).is_err());
        assert!(
            run(&f.args).is_err(),
            "Prepare must not silently replace original ownership"
        );
    }
    #[test]
    fn unsafe_invocation_filename_is_rejected_before_reservation() {
        let mut f = fixture();
        f.invocation.invocation_id = "../outside".into();
        f.args[4] = serde_json::to_string(&f.invocation).unwrap();
        assert!(run(&f.args).is_err());
        assert!(!f.root.path().join("provider/owner").exists());
    }
}
