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
    service_authority: v2::ServiceAuthority,
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
    ensure!(
        graph.kind == "theseus_native_integration_fixture_graph" && graph.schema == 1,
        "unsupported fixture graph"
    );
    let _pins = [
        pin(&graph.launch)?,
        pin(&graph.config)?,
        pin(&graph.provider_template)?,
    ];
    ensure!(
        std::env::current_exe()? == graph.controller.path,
        "fixture controller differs from pinned image"
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
    helper.args([
        std::ffi::OsStr::new("serve"),
        std::ffi::OsStr::new("--config"),
        graph.config.path.as_os_str(),
    ]);
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
    let authority = LaunchAuthority::fixture_integration(
        helper,
        StoreBinding {
            binding: v2::BindingId::parse(&graph.binding)?,
            manifest_digest: v2::Digest::parse(&graph.manifest_digest)?,
            workspace_marker: v2::WorkspaceMarker::parse(&graph.workspace_marker)?,
            os,
        },
        v2::Digest::parse(&graph.provider_epoch)?,
        v2::Digest::parse(&graph.core_manifest_digest)?,
        v2::Digest::parse(&graph.integration_manifest_digest)?,
        graph.service_authority,
    )?
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
