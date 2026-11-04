//! Installer effects belong to the compiled SUT, never to the helper service API.
#[path = "native_service_definition.rs"]
mod definition;
#[cfg(target_os = "linux")]
#[path = "native_service_sut.rs"]
mod sut;
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

#[cfg(target_os = "linux")]
pub fn run(args: &[String]) -> Result<()> {
    ensure!(args.len() == 5, "expected action/config/base/target/fault");
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
    } = &manifest.service_authority
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
        manager_unique_name: context.manager_unique_name.clone(),
        manager_version: qualification.version.clone(),
        unit: unit.clone(),
        definition_root: definition_root.into(),
        unit_load_paths: unit_load_roots.clone(),
        ownership_id: context.ownership_id.clone(),
        context_receipt: context.namespace_receipt.clone(),
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
            whole_private_context_receipt: context.namespace_receipt,
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
            std::fs::copy(source, &reader)?;
            std::fs::write(workspace.join("app"), b"theseus fixture app\n")?;
            if args[4] != "missing_service" {
                manager.install(&format!("{role}-{role}"), deadline, &cancel)?;
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

#[cfg(target_os = "macos")]
#[path = "native_service_launchd_sut.rs"]
mod platform_sut;
#[cfg(windows)]
#[path = "native_service_scm_sut.rs"]
mod platform_sut;

#[cfg(any(target_os = "macos", windows))]
pub fn run(args: &[String]) -> Result<()> {
    ensure!(args.len() == 5, "expected action/config/base/target/fault");
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
            let v2::ServiceAuthority::Scm { preflight, .. } = &manifest.service_authority else {
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
    #[cfg(target_os = "macos")]
    let mut manager =
        platform_sut::NativeServiceSut::connect(&manifest, &context, deadline, &cancel)?;
    #[cfg(windows)]
    let mut manager =
        platform_sut::NativeServiceSut::connect(&manifest, &context, &args[4], deadline, &cancel)?;
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
            // Clean's pre-proved private namespace must be empty; never replace a
            // foreign installed reader as an installer cleanup shortcut.
            ensure!(
                !reader.try_exists()?,
                "preexisting fixture reader cannot be adopted"
            );
            std::fs::copy(source, &reader)?;
            std::fs::write(workspace.join("app"), b"theseus fixture app\n")?;
            if args[4] != "missing_service" {
                manager.install(choose(role, role)?, deadline, &cancel)?;
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
