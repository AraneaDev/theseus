//! Test provider protocol. A retained test-owned operator owns the broker child.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::Path,
    time::{Duration, Instant},
};
use theseus::native::model::{ProviderAction, ProviderInvocation, ProviderReceipt};
use theseus_host::fs::{HostDir, RootPolicy};
use theseus_native_helper_store::{BootstrapRequest, LedgerStore, bootstrap_declared_integration};
use theseus_native_protocol::v2;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorRequest {
    pub ownership_id: String,
    pub invocation_id: String,
    pub action: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorReply {
    pub ownership_id: String,
    pub invocation_id: String,
    pub action: String,
    pub success: bool,
    pub detail: String,
}
pub fn publish(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= 1_048_576, "source record bound");
    let temporary = path.with_extension("publishing");
    let mut file = std::fs::File::create_new(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temporary, path)?;
    Ok(())
}
fn operator(root: &Path, invocation: &ProviderInvocation, action: &str) -> Result<()> {
    let request = OperatorRequest {
        ownership_id: invocation.ownership_id.clone(),
        invocation_id: invocation.invocation_id.clone(),
        action: action.into(),
    };
    let path = root.join(format!("{}.operator-request", invocation.invocation_id));
    publish(&path, &request)?;
    let response = root.join(format!("{}.operator-reply", invocation.invocation_id));
    let until = Instant::now() + Duration::from_secs(45);
    loop {
        match std::fs::read(&response) {
            Ok(bytes) => {
                ensure!(bytes.len() <= 16384, "operator response bound");
                let reply: OperatorReply = serde_json::from_slice(&bytes)?;
                ensure!(
                    reply.ownership_id == request.ownership_id
                        && reply.invocation_id == request.invocation_id
                        && reply.action == request.action
                        && reply.success,
                    "original broker operator: {}",
                    reply.detail
                );
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                ensure!(
                    Instant::now() < until,
                    "original broker operator reply pending"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(e.into()),
        }
    }
}
/// Attach only the original broker sidecar; never invoke another provider or helper bootstrap.
pub(crate) fn prepare_broker_sidecar(
    root: &Path,
    invocation: &ProviderInvocation,
    context: &v2::IntegrationRuntimeContext,
) -> Result<()> {
    let header = context
        .desktop_broker_header
        .as_ref()
        .context("original broker header missing")?;
    header.validate()?;
    ensure!(
        header.ownership_id == invocation.ownership_id
            && context.ownership_id == invocation.ownership_id
            && matches!(
                header.display,
                v2::broker::SuppliedDisplay::SourceOwnedHeadless
            )
            && header.allowed_modes == [v2::broker::DesktopMode::OwnedExecutable],
        "source sidecar requires exact original owned-only header"
    );
    let parent = root.parent().context("fixture parent")?;
    let ledger = HostDir::open(
        &parent.join("broker-ledger"),
        RootPolicy::AllComponentsNoFollow,
    )?;
    let workspace = HostDir::open(
        &parent.join("broker-workspace"),
        RootPolicy::AllComponentsNoFollow,
    )?;
    LedgerStore::bootstrap_broker(&ledger, &workspace, header)?;
    let broker_path = parent.join("broker.json");
    let mut broker: serde_json::Value = serde_json::from_slice(&std::fs::read(&broker_path)?)?;
    broker["header"] = serde_json::to_value(header)?;
    publish(&broker_path, &broker)?;
    operator(root, invocation, "start")
}
pub(crate) fn stop_broker_sidecar(root: &Path, invocation: &ProviderInvocation) -> Result<()> {
    operator(root, invocation, "stop")
}
pub fn run(args: &[String]) -> Result<()> {
    ensure!(
        args.len() == 5,
        "desktop provider expects root/template/config/mode/invocation"
    );
    let root = Path::new(&args[0]);
    let _root = HostDir::open(root, RootPolicy::AllComponentsNoFollow)?;
    let invocation: ProviderInvocation = serde_json::from_str(&args[4])?;
    invocation.validate()?;
    ensure!(
        invocation.provider == "integration_fixture_provider",
        "original fixture provider"
    );
    for id in [&invocation.ownership_id, &invocation.invocation_id] {
        ensure!(
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
            "safe original invocation ID"
        );
    }
    let saved = root.join(format!("{}.json", invocation.invocation_id));
    if args[3] == "query" {
        match std::fs::read(saved) {
            Ok(bytes) => {
                let record: serde_json::Value = serde_json::from_slice(&bytes)?;
                ensure!(
                    record["invocation"] == serde_json::to_value(&invocation)?,
                    "query original invocation only"
                );
                print!("{}", record["receipt"]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => print!("null"),
            Err(e) => return Err(e.into()),
        };
        return Ok(());
    }
    ensure!(args[3] == "invoke", "provider mode");
    let reservation = root.join("owner");
    let mut receipt: ProviderReceipt = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    receipt.provider = invocation.provider.clone();
    receipt.invocation_id = invocation.invocation_id.clone();
    receipt.invocation_digest = invocation.digest()?;
    if invocation.action == ProviderAction::Prepare {
        let mut owner = std::fs::File::create_new(&reservation)?;
        owner.write_all(invocation.ownership_id.as_bytes())?;
        owner.sync_all()?;
        let config_path = Path::new(&args[2]);
        let mut config: serde_json::Value = serde_json::from_slice(&std::fs::read(config_path)?)?;
        let manifest: v2::IntegrationManifest =
            serde_json::from_value(config["integration_manifest"].clone())?;
        let mut context: v2::IntegrationRuntimeContext =
            serde_json::from_value(config["runtime_context"].clone())?;
        ensure!(
            manifest.service_authority.is_none(),
            "desktop provider has no manager authority"
        );
        context.ownership_id = invocation.ownership_id.clone();
        context
            .desktop_broker_header
            .as_mut()
            .context("broker header")?
            .ownership_id = invocation.ownership_id.clone();
        manifest.validate_context(&context, v2::Os::Linux)?;
        let field = |name: &str| {
            config[name]
                .as_str()
                .with_context(|| format!("fixture field {name}"))
        };
        let ledger = HostDir::open(
            Path::new(field("ledger_root")?),
            RootPolicy::AllComponentsNoFollow,
        )?;
        let workspace = HostDir::open(
            Path::new(field("workspace_root")?),
            RootPolicy::AllComponentsNoFollow,
        )?;
        let commitment = v2::artifact_commitment(
            config["artifacts"]["base"]["length"]
                .as_u64()
                .context("base length")?,
            v2::Digest::parse(
                config["artifacts"]["base"]["sha256"]
                    .as_str()
                    .context("base hash")?,
            )?,
            config["artifacts"]["target"]["length"]
                .as_u64()
                .context("target length")?,
            v2::Digest::parse(
                config["artifacts"]["target"]["sha256"]
                    .as_str()
                    .context("target hash")?,
            )?,
        )?;
        bootstrap_declared_integration(
            &ledger,
            &workspace,
            BootstrapRequest {
                binding: v2::BindingId::parse(field("binding")?)?,
                manifest_digest: v2::Digest::parse(field("manifest_digest")?)?,
                os: v2::Os::Linux,
            },
            &context,
            &manifest,
            commitment,
        )?;
        config["runtime_context"] = serde_json::to_value(&context)?;
        publish(config_path, &config)?;
        prepare_broker_sidecar(root, &invocation, &context)?;
        receipt.receipt = serde_json::to_string(&context)?;
    } else {
        ensure!(
            std::fs::read(&reservation)? == invocation.ownership_id.as_bytes(),
            "original provider owner missing"
        );
        stop_broker_sidecar(root, &invocation)?;
        let mut epoch = [0; 32];
        theseus_host::random_bytes(&mut epoch)?;
        receipt.reset_epoch = v2::Digest::from_bytes(epoch).to_hex();
        receipt.receipt = format!(
            "source-desktop-original-broker-joined-{}",
            invocation.invocation_id
        );
        std::fs::remove_file(reservation)?;
    }
    receipt.validate()?;
    publish(
        &saved,
        &serde_json::json!({"invocation":invocation,"receipt":receipt}),
    )?;
    print!("{}", serde_json::to_string(&receipt)?);
    Ok(())
}
