//! Test-only product entry: trusted fixture graph supplies one prevalidated authority.
//! No shipping launch parser accepts a local executable command.
use anyhow::{Result, ensure};
use clap::Parser;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::{io::Read, path::PathBuf};
use theseus::{
    native::launch::LaunchAuthority,
    target_authority::{FixtureExecutablePins, RetainedLaunchFile, TargetAuthorityLoader},
};
use theseus_host::{
    fs::{HostDir, RelativePath, RootPolicy, fingerprint},
    process::{ChildEnvironment, NativeCommand},
};
use theseus_native_helper_store::StoreBinding;
use theseus_native_protocol::v2;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pin {
    path: PathBuf,
    sha256: String,
    length: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Graph {
    kind: String,
    schema: u32,
    launch: Pin,
    // Original input fidelity is enforced by snapshot capture/export. This is
    // decode-only metadata: fixtures may reconstruct recipes before execution.
    #[serde(default, rename = "recipe")]
    _recipe: Option<Pin>,
    controller: Pin,
    base: Pin,
    target: Pin,
    helper: Pin,
    config: Pin,
    provider: Pin,
    provider_template: Pin,
    provider_root: PathBuf,
    binding: String,
    manifest_digest: String,
    workspace_marker: String,
    provider_epoch: String,
    core_manifest_digest: String,
    integration_manifest_digest: String,
    service_authority: Option<v2::ServiceAuthority>,
    desktop_authority: Option<v2::broker::DesktopAuthority>,
    source_mode: Option<String>,
    broker: Option<Pin>,
    broker_config: Option<Pin>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceGraphMode {
    Service,
    Desktop,
    Combined,
}
fn source_graph_mode(graph: &Graph) -> Result<SourceGraphMode> {
    let service = graph.service_authority.is_some();
    let desktop = graph.desktop_authority.is_some();
    let broker = graph.broker.is_some() && graph.broker_config.is_some();
    match graph.source_mode.as_deref() {
        None => {
            ensure!(
                service && !desktop && graph.broker.is_none() && graph.broker_config.is_none(),
                "service graph authority pairing changed"
            );
            Ok(SourceGraphMode::Service)
        }
        Some("source_owned_desktop" | "source_owned_combined") => {
            let combined = graph.source_mode.as_deref() == Some("source_owned_combined");
            ensure!(
                desktop && broker && service == combined,
                "compiled source graph requires exact authority and independent broker pins"
            );
            let authority = graph.desktop_authority.as_ref().unwrap();
            authority.validate()?;
            ensure!(
                !combined || authority.os == "linux",
                "combined source SUT fixture requires supplied Linux manager; other platform acceptance is separate"
            );
            ensure!(
                matches!(
                    authority.display,
                    v2::broker::SuppliedDisplay::SourceOwnedHeadless
                ) && authority.allowed_modes == [v2::broker::DesktopMode::OwnedExecutable],
                "compiled Source graph has no native activation authority"
            );
            if let Some(service) = &graph.service_authority {
                service.validate()?;
            }
            Ok(if combined {
                SourceGraphMode::Combined
            } else {
                SourceGraphMode::Desktop
            })
        }
        _ => anyhow::bail!("unsupported compiled fixture source mode"),
    }
}
struct RetainedPin {
    _directory: HostDir,
    _file: std::fs::File,
}
fn pin(pin: &Pin) -> Result<RetainedPin> {
    ensure!(
        pin.path.is_absolute(),
        "fixture pins require absolute paths"
    );
    let parent = pin
        .path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("pin parent missing"))?;
    let directory = HostDir::open(parent, RootPolicy::AllComponentsNoFollow)?;
    let leaf = RelativePath::parse(
        pin.path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid pin filename"))?,
    )?;
    let mut file = directory.open_file(&leaf)?;
    let before = fingerprint(&file)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    ensure!(
        format!("{:x}", hash.finalize()) == pin.sha256,
        "fixture pin digest mismatch"
    );
    ensure!(
        before == fingerprint(&file)? && before == fingerprint(&directory.open_file(&leaf)?)?,
        "fixture pin changed"
    );
    Ok(RetainedPin {
        _directory: directory,
        _file: file,
    })
}
struct Loader {
    source: RetainedLaunchFile,
    authority: LaunchAuthority,
}
impl TargetAuthorityLoader for Loader {
    fn load(&self, source: &RetainedLaunchFile) -> Result<LaunchAuthority> {
        ensure!(
            source.identity_matches(&self.source)?,
            "fixture accepts only original pinned launch file"
        );
        Ok(self.authority.clone())
    }
}
/// This process's own image path in plain absolute form. Rust's Windows
/// process spawning passes a verbatim (`\\?\C:\...`) application name, so
/// `current_exe` can report the verbatim spelling of the same drive path. Only
/// that exact lexical prefix is removed (never UNC/device forms, and only
/// when the remainder is already a normalized drive path); the result is
/// still compared exactly with the pinned image path.
fn current_image_path() -> Result<PathBuf> {
    let current = std::env::current_exe()?;
    #[cfg(windows)]
    if let Some(plain) = current.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        let bytes = plain.as_bytes();
        ensure!(
            bytes.len() > 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && bytes[2] == b'\\'
                && !plain[3..].split('\\').any(|c| {
                    // Verbatim paths skip Win32 normalization; refuse every
                    // form Win32 would reinterpret once the prefix is removed.
                    let stem = c.split('.').next().unwrap_or(c).to_ascii_uppercase();
                    c.is_empty()
                        || c == "."
                        || c == ".."
                        || c.contains(['/', ':'])
                        || c.ends_with(['.', ' '])
                        || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                            && stem.len() == 4
                            && stem.as_bytes()[3].is_ascii_digit())
                }),
            "fixture controller image path is not a plain drive path"
        );
        return Ok(PathBuf::from(plain));
    }
    Ok(current)
}
fn execute() -> Result<i32> {
    let mut args = std::env::args_os();
    let program = args.next().unwrap_or_default();
    ensure!(
        args.next().as_deref() == Some(std::ffi::OsStr::new("--fixture-graph")),
        "explicit fixture graph required"
    );
    let path = PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow::anyhow!("fixture graph path missing"))?,
    );
    let graph_source = RetainedLaunchFile::open(&path)?;
    let graph: Graph = serde_json::from_slice(graph_source.bytes())?;
    let source_mode = source_graph_mode(&graph)?;
    ensure!(
        graph.kind == "theseus_native_integration_fixture_graph" && graph.schema == 1,
        "unsupported fixture graph"
    );
    let _broker_config_pin = graph.broker_config.as_ref().map(pin).transpose()?;
    let _pins = [
        pin(&graph.launch)?,
        pin(&graph.config)?,
        pin(&graph.provider_template)?,
    ];
    let current = current_image_path()?;
    ensure!(
        current == graph.controller.path,
        "fixture controller differs from pinned image: running {:?}, pinned {:?}",
        current,
        graph.controller.path
    );
    let executables = std::sync::Arc::new(FixtureExecutablePins::retain(
        [
            &graph.controller,
            &graph.helper,
            &graph.provider,
            &graph.base,
            &graph.target,
        ]
        .into_iter()
        .chain(graph.broker.iter())
        .map(|p| (p.path.clone(), p.length, p.sha256.clone()))
        .collect(),
    )?);
    let source = RetainedLaunchFile::open(&graph.launch.path)?;
    ensure!(
        source.sha256() == graph.launch.sha256,
        "fixture launch digest mismatch"
    );
    ensure!(
        graph.provider_root.is_absolute(),
        "explicit fixture provider root required"
    );
    let _provider_root = HostDir::open(&graph.provider_root, RootPolicy::AllComponentsNoFollow)?;
    let mut helper = NativeCommand::new(&graph.helper.path);
    if source_mode != SourceGraphMode::Service {
        helper.arg("source-desktop-serve").arg(&graph.config.path);
    } else {
        helper.args([
            std::ffi::OsStr::new("serve"),
            std::ffi::OsStr::new("--config"),
            graph.config.path.as_os_str(),
        ]);
    }
    helper.environment = ChildEnvironment::Replace(vec![]);
    let mut provider = NativeCommand::new(&graph.provider.path);
    provider
        .arg("provider")
        .arg(&graph.provider_root)
        .arg(&graph.provider_template.path)
        .arg(&graph.config.path);
    provider.environment = ChildEnvironment::Replace(vec![]);
    let os = if cfg!(windows) {
        v2::Os::Windows
    } else if cfg!(target_os = "macos") {
        v2::Os::Macos
    } else {
        v2::Os::Linux
    };
    let binding = StoreBinding {
        binding: v2::BindingId::parse(&graph.binding)?,
        manifest_digest: v2::Digest::parse(&graph.manifest_digest)?,
        workspace_marker: v2::WorkspaceMarker::parse(&graph.workspace_marker)?,
        os,
    };
    let epoch = v2::Digest::parse(&graph.provider_epoch)?;
    let core = v2::Digest::parse(&graph.core_manifest_digest)?;
    let integration = v2::Digest::parse(&graph.integration_manifest_digest)?;
    let authority = if source_mode == SourceGraphMode::Combined {
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&graph.config.path)?)?;
        let manifest: v2::IntegrationManifest =
            serde_json::from_value(config["integration_manifest"].clone())?;
        LaunchAuthority::fixture_combined(
            helper,
            binding,
            epoch,
            core,
            integration,
            graph
                .service_authority
                .ok_or_else(|| anyhow::anyhow!("combined service ceiling missing"))?,
            graph
                .desktop_authority
                .ok_or_else(|| anyhow::anyhow!("combined desktop ceiling missing"))?,
            &manifest,
        )?
    } else if source_mode == SourceGraphMode::Desktop {
        LaunchAuthority::fixture_desktop(
            helper,
            binding,
            epoch,
            core,
            integration,
            graph
                .desktop_authority
                .ok_or_else(|| anyhow::anyhow!("desktop ceiling missing"))?,
        )?
    } else {
        LaunchAuthority::fixture_integration(
            helper,
            binding,
            epoch,
            core,
            integration,
            graph
                .service_authority
                .ok_or_else(|| anyhow::anyhow!("service ceiling missing"))?,
        )?
    }
    .with_fixture_provider("integration_fixture_provider".into(), provider)?
    .with_fixture_executable_pins(executables)?;
    let loader = Loader { source, authority };
    graph_source.verify()?;
    let first = args.next();
    if first.as_deref() == Some(std::ffi::OsStr::new("--fixture-fingerprint")) {
        ensure!(
            args.next().is_none(),
            "fixture fingerprint takes no product arguments"
        );
        println!("{}", loader.authority.view().fingerprint);
        return Ok(0);
    }
    let product_args = std::iter::once(program).chain(first).chain(args);
    theseus::cli::execute_with_loader(theseus::cli::Cli::parse_from(product_args), &loader)
}
fn main() {
    let code = match execute() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("integration fixture: {error:#}");
            2
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod source_graph_tests {
    use super::*;
    fn combined_raw() -> serde_json::Value {
        let pin = serde_json::json!({"path":"/fixture/image","sha256":"11".repeat(32),"length":1});
        let service = serde_json::json!({"kind":"systemd","slot":"service","scope":"user","bus_endpoint":{"kind":"unix_path","path":"/fixture/bus"},"qualification":{"manager":"systemd","version":"262","api_profile":"systemd_dbus_private_v1","features":["unit_properties","start_stop","manager_reload"],"receipt_sha256":"03".repeat(32)},"namespace":{"kind":"exclusive_owned_registration","slot":"service","unit_load_roots":["/fixture/units"],"dropins":"forbidden","provider_context":"source-fixture"},"manager_context":{"kind":"whole_private","slot":"manager","sut_allowed_actions":["daemon_reload"],"rehearsal_allowed_actions":[],"provider_receipt_required":true},"expected_peer_uid":1000,"expected_manager_uid":1000,"definition_root":"/fixture/units","unit":"theseus-fixture.service","managed_by":"installer","sut_effects":["registration","configuration","removal","daemon_reload"],"allowed_actions":["inspect","start","stop"]});
        let desktop = serde_json::json!({"target_id":"source-combined","os":"linux","session_id":"source-combined","session_epoch":"66".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"source_owned_headless"},"allowed_modes":["owned_executable"]});
        serde_json::json!({"kind":"theseus_native_integration_fixture_graph","schema":1,"launch":pin,"controller":pin,"base":pin,"target":pin,"helper":pin,"config":pin,"provider":pin,"provider_template":pin,"provider_root":"/fixture/provider","binding":"01".repeat(16),"manifest_digest":"11".repeat(32),"workspace_marker":"02".repeat(16),"provider_epoch":"22".repeat(32),"core_manifest_digest":"33".repeat(32),"integration_manifest_digest":"44".repeat(32),"service_authority":service,"desktop_authority":desktop,"source_mode":"source_owned_combined","broker":pin,"broker_config":pin})
    }
    fn combined() -> Graph {
        serde_json::from_value(combined_raw()).unwrap()
    }
    #[test]
    fn original_recipe_pin_is_optional_decode_only_fixture_metadata() {
        let legacy: Graph = serde_json::from_value(combined_raw()).unwrap();
        assert!(source_graph_mode(&legacy).is_ok());
        let mut captured = combined_raw();
        captured["recipe"] = serde_json::json!({"path":"/fixture/recipe.json","sha256":"aa".repeat(32),"length":123});
        let current: Graph = serde_json::from_value(captured)
            .expect("new original input metadata must decode without changing fixture authority");
        assert!(source_graph_mode(&current).is_ok());
    }
    #[test]
    fn source_combined_graph_requires_both_authorities_exact_owned_mode_and_broker_pins() {
        assert!(source_graph_mode(&combined()).is_ok());
        let mut bad = combined();
        bad.service_authority = None;
        assert!(source_graph_mode(&bad).is_err());
        let mut bad = combined();
        bad.desktop_authority = None;
        assert!(source_graph_mode(&bad).is_err());
        let mut bad = combined();
        bad.broker = None;
        assert!(source_graph_mode(&bad).is_err());
        let mut bad = combined();
        bad.broker_config = None;
        assert!(source_graph_mode(&bad).is_err());
        let mut bad = combined();
        bad.desktop_authority.as_mut().unwrap().allowed_modes =
            vec![v2::broker::DesktopMode::NativeActivation];
        assert!(source_graph_mode(&bad).is_err());
        let mut bad = combined();
        bad.source_mode = Some("native_activation".into());
        assert!(source_graph_mode(&bad).is_err());
    }
}
