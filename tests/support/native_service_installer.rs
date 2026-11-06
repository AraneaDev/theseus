//! Installer effects belong to the compiled SUT, never to the helper service API.
#[cfg(windows)]
#[path = "native_service_acl.rs"]
#[allow(unused_imports)] // This SUT uses the shared ACL module without its setup-only probe.
mod acl;
#[path = "native_service_definition.rs"]
mod definition;
#[cfg(target_os = "linux")]
#[path = "native_service_sut.rs"]
mod sut;
#[cfg(any(target_os = "linux", windows))]
use anyhow::Context;
use anyhow::{Result, ensure};
use sha2::{Digest as _, Sha256};
use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
#[cfg(target_os = "linux")]
use theseus_host::services::systemd::{SystemdAuthority, SystemdScope, UnixPath};
use theseus_native_protocol::v2;

/// A replacement deletes only the exact source-owned reader pinned before the
/// SUT stops its service. Frozen executable modes never need a content-write
/// grant, and a same-name/same-byte replacement cannot inherit this authority.
#[cfg(unix)]
struct PinnedUnixReader {
    root: theseus_host::fs::HostDir,
    leaf: std::ffi::OsString,
    file: std::fs::File,
    fingerprint: theseus_host::fs::FileFingerprint,
}
#[cfg(unix)]
impl PinnedUnixReader {
    fn pin(path: &Path, length: u64, digest: &str) -> Result<Self> {
        use std::io::Read;
        use theseus_host::fs::{HostDir, LeafOpenResult, RootPolicy, fingerprint};
        let root = HostDir::open(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("reader parent missing"))?,
            RootPolicy::AllComponentsNoFollow,
        )?;
        let leaf = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("reader leaf missing"))?
            .to_owned();
        let mut file = match root.open_leaf_classified(&leaf) {
            LeafOpenResult::Opened(file) => file,
            other => anyhow::bail!("original reader unavailable: {other:?}"),
        };
        let original = fingerprint(&file)?;
        ensure!(
            original.len == length && length <= 64 * 1024 * 1024,
            "original reader length differs"
        );
        let mut hash = Sha256::new();
        let mut bytes = [0u8; 16384];
        let mut read = 0u64;
        loop {
            let count = file.read(&mut bytes)?;
            if count == 0 {
                break;
            }
            read += count as u64;
            ensure!(read <= length, "original reader grew during pinning");
            hash.update(&bytes[..count]);
        }
        ensure!(
            read == length
                && format!("{:x}", hash.finalize()) == digest
                && fingerprint(&file)? == original,
            "original reader changed during pinning"
        );
        Ok(Self {
            root,
            leaf,
            file,
            fingerprint: original,
        })
    }
    fn remove(self) -> Result<()> {
        ensure!(
            theseus_host::fs::fingerprint(&self.file)? == self.fingerprint,
            "original reader changed while service stopped"
        );
        self.root.remove_created_leaf(&self.leaf, &self.file)?;
        Ok(())
    }
}
#[cfg(target_os = "linux")]
pub fn run(args: &[String]) -> Result<()> {
    ensure!(args.len() == 5, "expected action/config/base/target/fault");
    ensure!(
        !(args[0] == "configure" && args[4] == "configure_fails"),
        "explicit fixture Configure failure before any mutation"
    );
    ensure!(
        [
            "none",
            "missing_service",
            "leaked_service",
            "wrong_version",
            "wrong_data",
            "wrong_nonce",
            "wrong_peer"
        ]
        .contains(&args[4].as_str()),
        "unknown fixture fault"
    );
    let config: serde_json::Value = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    let field = |name: &str| -> Result<&str> {
        config[name]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("fixture config missing {name}"))
    };
    ensure!(
        config["schema"] == 3 && field("profile")? == "native_integration_v1",
        "explicit integration fixture config required"
    );
    let manifest: v2::IntegrationManifest =
        serde_json::from_value(config["integration_manifest"].clone())?;
    let context: v2::IntegrationRuntimeContext =
        serde_json::from_value(config["runtime_context"].clone())?;
    manifest.validate()?;
    context.validate()?;
    let integration = manifest.digest()?;
    ensure!(
        integration.to_hex() == field("integration_manifest_digest")?
            && context.manifest_digest == field("manifest_digest")?
            && v2::integration_manifest_digest(
                v2::Digest::parse(&manifest.core_manifest_digest)?,
                integration
            )
            .to_hex()
                == context.manifest_digest,
        "fixture manifest context mismatch"
    );
    let workspace = Path::new(field("workspace_root")?);
    ensure!(
        workspace.is_absolute(),
        "explicit fixture workspace required"
    );
    let reader = workspace.join("reader");
    let v2::ServiceAuthority::Systemd {
        scope,
        bus_endpoint,
        expected_peer_uid,
        expected_manager_uid,
        qualification,
        namespace,
        definition_root,
        unit,
        ..
    } = manifest
        .service_authority
        .as_ref()
        .context("service authority missing")?
    else {
        anyhow::bail!("systemd fixture authority required")
    };
    let v2::LocalEndpoint::UnixPath { path } = bus_endpoint else {
        anyhow::bail!("systemd Unix endpoint required")
    };
    let v2::RegistrationNamespace::ExclusiveOwnedRegistration {
        unit_load_roots, ..
    } = namespace;
    let authority = SystemdAuthority {
        endpoint: UnixPath::new(path.clone())?,
        scope: match scope {
            v2::ServiceScope::User => SystemdScope::User,
            v2::ServiceScope::System => SystemdScope::System,
        },
        bus_uid: *expected_peer_uid,
        manager_uid: *expected_manager_uid,
        manager_unique_name: context
            .manager_unique_name
            .clone()
            .context("service manager name missing")?,
        manager_version: qualification.version.clone(),
        unit: unit.clone(),
        definition_root: definition_root.into(),
        unit_load_paths: unit_load_roots.clone(),
        ownership_id: context.ownership_id.clone(),
        context_receipt: context
            .namespace_receipt
            .clone()
            .context("service namespace receipt missing")?,
        allowed_start: false,
        allowed_stop: false,
    };
    let definitions = manifest
        .definitions
        .iter()
        .map(|d| {
            let bytes = definition::definition_bytes(d);
            ensure!(
                format!("{:x}", Sha256::digest(&bytes)) == d.definition_sha256,
                "fixture definition byte commitment differs"
            );
            Ok(sut::SutDefinition {
                id: format!("{}-{}", d.role, d.artifact),
                bytes,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut manager = sut::NativeServiceSut::connect(
        sut::SutAuthority {
            manager: authority,
            whole_private_context_receipt: context
                .namespace_receipt
                .context("service namespace receipt missing")?,
        },
        definitions,
        deadline,
        &cancel,
    )?;
    match args[0].as_str() {
        "install_base" | "install_target" => {
            let base = args[0] == "install_base";
            let role = if base { "base" } else { "target" };
            let source = Path::new(if base { &args[2] } else { &args[3] });
            let bytes = std::fs::read(source)?;
            let expected = &config["artifacts"][role];
            ensure!(
                expected["length"].as_u64() == Some(bytes.len() as u64)
                    && expected["sha256"].as_str()
                        == Some(format!("{:x}", Sha256::digest(&bytes)).as_str()),
                "fixture source artifact commitment differs"
            );
            let repeating = reader.try_exists()?;
            if repeating {
                ensure!(!base, "base install cannot replace an existing reader");
                let current = std::fs::read(&reader)?;
                let digest = format!("{:x}", Sha256::digest(&current));
                ensure!(
                    ["base", "target"]
                        .iter()
                        .any(|role| config["artifacts"][*role]["sha256"].as_str()
                            == Some(digest.as_str())),
                    "foreign reader cannot be adopted"
                );
                ensure!(
                    std::fs::read(workspace.join("data"))? == b"theseus fixture data\n"
                        && std::fs::read(workspace.join("app"))? == b"theseus fixture app\n",
                    "original configured app/data required"
                );
                let pinned = PinnedUnixReader::pin(&reader, current.len() as u64, &digest)?;
                manager.stop_for_transition(deadline, &cancel)?;
                pinned.remove()?;
            }
            std::fs::copy(source, &reader)?;
            if repeating {
                manager.configure("configured-target", deadline, &cancel)?;
                manager.start_after_transition(deadline, &cancel)?;
            } else {
                std::fs::write(workspace.join("app"), b"theseus fixture app\n")?;
                if args[4] != "missing_service" {
                    manager.install(&format!("{role}-{role}"), deadline, &cancel)?;
                }
            }
        }
        "configure" => {
            std::fs::write(workspace.join("data"), b"theseus fixture data\n")?;
            if args[4] != "missing_service" {
                let digest = format!("{:x}", Sha256::digest(std::fs::read(&reader)?));
                let role = if config["artifacts"]["base"]["sha256"].as_str() == Some(&digest) {
                    "base"
                } else if config["artifacts"]["target"]["sha256"].as_str() == Some(&digest) {
                    "target"
                } else {
                    anyhow::bail!("installed fixture artifact differs")
                };
                manager.configure(&format!("configured-{role}"), deadline, &cancel)?;
            }
        }
        "uninstall" => {
            if args[4] == "leaked_service" {
                return Ok(());
            }
            if args[4] != "leaked_service" {
                manager.uninstall(deadline, &cancel)?;
            }
            for path in [reader, workspace.join("app"), workspace.join("data")] {
                match std::fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        _ => anyhow::bail!("unknown fixture SUT action"),
    }
    Ok(())
}

#[cfg(any(target_os = "macos", windows))]
#[path = "native_service_owner_bridge.rs"]
mod owner_bridge;
#[cfg(windows)]
#[path = "native_service_scm_sut.rs"]
#[allow(dead_code)]
mod platform_sut;

#[cfg(any(target_os = "macos", windows))]
pub fn run(args: &[String]) -> Result<()> {
    ensure!(
        args.len() == 6,
        "expected action/config/base/target/fault/original-operator-root"
    );
    ensure!(
        !(args[0] == "configure" && args[4] == "configure_fails"),
        "explicit fixture Configure failure before any mutation"
    );
    ensure!(
        ["install_base", "install_target", "configure", "uninstall"].contains(&args[0].as_str()),
        "unknown fixture SUT action"
    );
    ensure!(
        [
            "none",
            "missing_service",
            "leaked_service",
            "wrong_version",
            "wrong_data",
            "wrong_nonce",
            "wrong_peer"
        ]
        .contains(&args[4].as_str()),
        "unknown fixture fault"
    );
    let config: serde_json::Value = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    let field = |name: &str| -> Result<&str> {
        config[name]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("fixture config missing {name}"))
    };
    ensure!(
        config["schema"] == 3 && field("profile")? == "native_integration_v1",
        "explicit integration fixture config required"
    );
    let manifest: v2::IntegrationManifest =
        serde_json::from_value(config["integration_manifest"].clone())?;
    let context: v2::IntegrationRuntimeContext =
        serde_json::from_value(config["runtime_context"].clone())?;
    #[cfg(target_os = "macos")]
    let os = v2::Os::Macos;
    #[cfg(windows)]
    let os = v2::Os::Windows;
    manifest.validate_context(&context, os)?;
    ensure!(
        manifest.digest()?.to_hex() == field("integration_manifest_digest")?
            && context.manifest_digest == field("manifest_digest")?
            && v2::integration_manifest_digest(
                v2::Digest::parse(&manifest.core_manifest_digest)?,
                manifest.digest()?
            )
            .to_hex()
                == context.manifest_digest,
        "fixture manifest context mismatch"
    );
    let workspace = Path::new(field("workspace_root")?);
    ensure!(
        workspace.is_absolute(),
        "explicit fixture workspace required"
    );
    #[cfg(target_os = "macos")]
    let reader = workspace.join("reader");
    #[cfg(windows)]
    let reader = workspace.join("reader.exe");
    let data = workspace.join("data");
    let readiness = manifest
        .readiness
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("fixture readiness missing"))?;
    let instance = readiness
        .cooperative_instance
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("provider-attested immutable instance required"))?;
    // Every platform definition commits exactly the installed foreground reader,
    // original provider input, data path, and explicitly selected fault.
    for d in &manifest.definitions {
        ensure!(
            Path::new(&d.executable) == reader
                && Path::new(&d.working_directory) == workspace
                && d.environment.is_empty(),
            "fixture definition executable/workspace/environment differs"
        );
        #[cfg(target_os = "macos")]
        {
            let v2::LocalEndpoint::UnixPath { path } = &readiness.endpoint else {
                anyhow::bail!("launchd fixture Unix endpoint required")
            };
            ensure!(
                d.arguments
                    == [
                        reader.display().to_string(),
                        "service".into(),
                        path.clone(),
                        data.display().to_string(),
                        args[4].clone(),
                        instance.clone()
                    ],
                "launchd fixture immutable argv differs"
            );
        }
        #[cfg(windows)]
        {
            let v2::ServiceAuthority::Scm { preflight, .. } =
                manifest
                    .service_authority
                    .as_ref()
                    .context("service authority missing")?
            else {
                anyhow::bail!("SCM fixture authority required")
            };
            ensure!(
                Path::new(&preflight.executable) == reader
                    && Path::new(&preflight.data) == data
                    && d.arguments == [d.executable.clone()],
                "SCM fixture explicit paths/argv differ"
            );
            let _ = instance; // manifest.validate_context already attests its hash
        }
    }
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    // The catalog request reaches the fixture's ORIGINAL creator owner. Each
    // installer process exits without losing or reconstructing that authority.
    let mut manager = owner_bridge::Client::connect(Path::new(&args[5]), &context)?;
    let choose = |role: &str, artifact: &str| -> Result<&v2::ServiceDefinition> {
        manifest
            .definitions
            .iter()
            .find(|d| d.role == role && d.artifact == artifact)
            .ok_or_else(|| anyhow::anyhow!("fixture definition missing {role}/{artifact}"))
    };
    match args[0].as_str() {
        "install_base" | "install_target" => {
            let base = args[0] == "install_base";
            let role = if base { "base" } else { "target" };
            let source = Path::new(if base { &args[2] } else { &args[3] });
            let bytes = std::fs::read(source)?;
            let expected = &config["artifacts"][role];
            ensure!(
                expected["length"].as_u64() == Some(bytes.len() as u64)
                    && expected["sha256"].as_str()
                        == Some(format!("{:x}", Sha256::digest(&bytes)).as_str()),
                "fixture source artifact commitment differs"
            );
            let repeating = reader.try_exists()?;
            if repeating {
                ensure!(!base, "base install cannot replace existing reader");
                let original = std::fs::read(&reader)?;
                let digest = format!("{:x}", Sha256::digest(&original));
                ensure!(
                    ["base", "target"]
                        .iter()
                        .any(|role| config["artifacts"][*role]["sha256"].as_str()
                            == Some(digest.as_str())),
                    "foreign reader cannot be adopted"
                );
                ensure!(
                    std::fs::read(&data)? == b"theseus fixture data\n"
                        && std::fs::read(workspace.join("app"))? == b"theseus fixture app\n",
                    "original configured app/data required"
                );
                #[cfg(unix)]
                let pinned = PinnedUnixReader::pin(&reader, original.len() as u64, &digest)?;
                #[cfg(windows)]
                let pinned =
                    platform_sut::PinnedReader::pin(&reader, original.len() as u64, &digest)?;
                manager.stop_for_transition(deadline, &cancel)?;
                #[cfg(unix)]
                pinned.remove()?;
                #[cfg(windows)]
                pinned.remove()?;
            }
            std::fs::copy(source, &reader)?;
            if repeating {
                manager.configure(choose("configured", "target")?, deadline, &cancel)?;
                manager.start_after_transition(deadline, &cancel)?;
            } else {
                std::fs::write(workspace.join("app"), b"theseus fixture app\n")?;
                if args[4] != "missing_service" {
                    manager.install(choose(role, role)?, deadline, &cancel)?;
                }
            }
        }
        "configure" => {
            std::fs::write(&data, b"theseus fixture data\n")?;
            if args[4] != "missing_service" {
                let digest = format!("{:x}", Sha256::digest(std::fs::read(&reader)?));
                let artifact = if config["artifacts"]["base"]["sha256"].as_str() == Some(&digest) {
                    "base"
                } else if config["artifacts"]["target"]["sha256"].as_str() == Some(&digest) {
                    "target"
                } else {
                    anyhow::bail!("installed fixture artifact differs")
                };
                manager.configure(choose("configured", artifact)?, deadline, &cancel)?;
            }
        }
        "uninstall" => {
            if args[4] == "leaked_service" {
                return Ok(());
            }
            if args[4] != "leaked_service" {
                manager.uninstall(deadline, &cancel)?;
                #[cfg(windows)]
                manager.remove_sidecar()?;
            }
            #[cfg(windows)]
            {
                let pinned = ["base", "target"]
                    .into_iter()
                    .map(|role| -> Result<(u64, String)> {
                        Ok((
                            config["artifacts"][role]["length"]
                                .as_u64()
                                .ok_or_else(|| {
                                    anyhow::anyhow!("reader length commitment missing")
                                })?,
                            config["artifacts"][role]["sha256"]
                                .as_str()
                                .ok_or_else(|| anyhow::anyhow!("reader hash commitment missing"))?
                                .to_owned(),
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?;
                platform_sut::remove_reader(&reader, &pinned)?;
            }
            #[cfg(target_os = "macos")]
            let remaining = vec![reader, workspace.join("app"), data];
            #[cfg(windows)]
            let remaining = vec![workspace.join("app"), data];
            for path in remaining {
                match std::fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        _ => unreachable!("validated fixture action"),
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod pinned_reader_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn frozen_reader_replacement_needs_no_content_write_and_preserves_data() {
        let root = tempfile::tempdir().unwrap();
        let reader = root.path().join("reader");
        let source = root.path().join("target");
        let data = root.path().join("data");
        std::fs::write(&reader, b"base").unwrap();
        std::fs::write(&source, b"target").unwrap();
        std::fs::write(&data, b"original-configured-data").unwrap();
        for path in [&reader, &source] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        let pinned =
            PinnedUnixReader::pin(&reader, 4, &format!("{:x}", Sha256::digest(b"base"))).unwrap();
        pinned.remove().unwrap();
        std::fs::copy(&source, &reader).unwrap();
        assert_eq!(std::fs::read(&reader).unwrap(), b"target");
        assert_eq!(
            std::fs::metadata(&reader).unwrap().permissions().mode() & 0o777,
            0o500
        );
        assert_eq!(std::fs::read(&data).unwrap(), b"original-configured-data");
    }
    #[test]
    fn pinned_reader_refuses_same_byte_foreign_replacement_after_stop() {
        let root = tempfile::tempdir().unwrap();
        let reader = root.path().join("reader");
        std::fs::write(&reader, b"base").unwrap();
        let pinned =
            PinnedUnixReader::pin(&reader, 4, &format!("{:x}", Sha256::digest(b"base"))).unwrap();
        std::fs::rename(&reader, root.path().join("original-owned-reader")).unwrap();
        std::fs::write(&reader, b"base").unwrap();
        assert!(pinned.remove().is_err());
        assert_eq!(std::fs::read(&reader).unwrap(), b"base");
        assert_eq!(
            std::fs::read(root.path().join("original-owned-reader")).unwrap(),
            b"base"
        );
    }
}
