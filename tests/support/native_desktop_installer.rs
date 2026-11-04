//! Compiled desktop SUT effects. The helper and broker never write entries.
use anyhow::{Context, Result, ensure};
use sha2::{Digest as _, Sha256};
use std::{
    io::{Read, Write},
    path::Path,
};
use theseus_host::fs::{HostDir, LeafOpenResult, RootPolicy, fingerprint};
use theseus_native_protocol::v2;
pub fn entry_bytes(manifest: &v2::IntegrationManifest) -> Result<Vec<u8>> {
    entry_bytes_for(
        manifest
            .desktop_executable
            .as_ref()
            .context("desktop executable missing")?,
    )
}
fn entry_bytes_for(executable: &v2::DesktopExecutable) -> Result<Vec<u8>> {
    ensure!(
        executable
            .arguments
            .iter()
            .all(|s| !s.contains(['\n', '\r', '\0', '%', '"', '\\'])
                && !s.chars().any(char::is_whitespace)),
        "fixture entry literal argv grammar"
    );
    Ok(format!("[Desktop Entry]\nType=Application\nName=Theseus source desktop\nTerminal=false\nDBusActivatable=false\nExec={}\n",executable.arguments.join(" ")).into_bytes())
}
fn remove_exact(path: &Path, allowed: &[Vec<u8>]) -> Result<()> {
    let root = HostDir::open(
        path.parent().context("leaf parent")?,
        RootPolicy::AllComponentsNoFollow,
    )?;
    let name = path.file_name().context("leaf name")?;
    let mut file = match root.open_leaf_classified(name) {
        LeafOpenResult::Opened(file) => file,
        LeafOpenResult::MissingSettled => return Ok(()),
        other => anyhow::bail!("original SUT leaf unavailable: {other:?}"),
    };
    let before = fingerprint(&file)?;
    ensure!(before.len <= 64 * 1024 * 1024, "fixture leaf bound");
    let mut bytes = vec![];
    (&mut file).take(before.len + 1).read_to_end(&mut bytes)?;
    ensure!(
        fingerprint(&file)? == before && allowed.iter().any(|a| a == &bytes),
        "foreign SUT leaf cannot be removed"
    );
    root.remove_created_leaf(name, &file)?;
    Ok(())
}
fn sut_definition_key(
    action: &str,
    reader: Option<v2::DesktopArtifactRole>,
    configured: bool,
) -> Result<v2::DesktopDefinitionKey> {
    use v2::{DesktopArtifactRole as A, DesktopDefinitionKey as K, DesktopDefinitionRole as R};
    match action {
        "install_base" => {
            ensure!(
                reader.is_none() && !configured,
                "base cannot adopt an existing generation"
            );
            Ok(K {
                role: R::Base,
                artifact: A::Base,
            })
        }
        "install_target" => {
            ensure!(
                reader.is_none() || configured,
                "replacement requires original configured data"
            );
            Ok(K {
                role: if reader.is_some() && configured {
                    R::Configured
                } else {
                    R::Target
                },
                artifact: A::Target,
            })
        }
        "configure" => Ok(K {
            role: R::Configured,
            artifact: reader.context("Configure requires original reader")?,
        }),
        _ => anyhow::bail!("unsupported fixture generation action"),
    }
}
fn retained_bytes(root: &HostDir, name: &str) -> Result<Option<Vec<u8>>> {
    let mut file = match root.open_leaf_classified(std::ffi::OsStr::new(name)) {
        LeafOpenResult::MissingSettled => return Ok(None),
        LeafOpenResult::Opened(file) => file,
        other => anyhow::bail!("fixture leaf authority differs: {other:?}"),
    };
    let before = fingerprint(&file)?;
    ensure!(before.len <= 64 * 1024 * 1024, "fixture leaf bound");
    let mut bytes = vec![];
    (&mut file).take(before.len + 1).read_to_end(&mut bytes)?;
    ensure!(fingerprint(&file)? == before, "fixture leaf changed");
    Ok(Some(bytes))
}
pub fn run(args: &[String]) -> Result<()> {
    ensure!(
        matches!(args.len(), 5 | 6),
        "desktop SUT expects action/config/base/target/fault plus explicit combined entry-only contract"
    );
    let entry_only = args.len() == 6;
    ensure!(
        !entry_only || args[5] == "--theseus-combined-entry-only",
        "unknown desktop fixture SUT mode"
    );
    ensure!(
        ["none", "missing_entry", "leaked_entry"].contains(&args[4].as_str()),
        "explicit desktop fixture fault required"
    );
    let config: serde_json::Value = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    let manifest: v2::IntegrationManifest =
        serde_json::from_value(config["integration_manifest"].clone())?;
    let context: v2::IntegrationRuntimeContext =
        serde_json::from_value(config["runtime_context"].clone())?;
    manifest.validate_context(&context, v2::Os::Linux)?;
    let desktop = manifest.desktop.as_ref().context("desktop manifest")?;
    ensure!(
        manifest.desktop_native.is_none()
            && desktop.allowed_modes == [v2::broker::DesktopMode::OwnedExecutable],
        "source installer has only explicitly owned executable authority"
    );
    if entry_only {
        let service = manifest
            .service_authority
            .as_ref()
            .context("combined service authority missing")?;
        ensure!(
            service.slot() != &desktop.slot
                && v2::IntegrationCapabilities::for_manifest(&manifest)?
                    == v2::IntegrationCapabilities::Combined,
            "combined entry-only requires exact distinct declared resource slots/capability"
        );
    } else {
        ensure!(
            manifest.service_authority.is_none(),
            "desktop-only SUT cannot acquire manager authority"
        );
    }
    let workspace = Path::new(config["workspace_root"].as_str().context("workspace")?);
    let root = HostDir::open(workspace, RootPolicy::AllComponentsNoFollow)?;
    let entries = HostDir::open(
        Path::new(&desktop.entry_root),
        RootPolicy::AllComponentsNoFollow,
    )?;
    let reader = workspace.join("reader");
    let entry = Path::new(&desktop.entry_root).join(&desktop.entry);
    let base = std::fs::read(&args[2])?;
    let target = std::fs::read(&args[3])?;
    let existing = retained_bytes(&root, "reader")?;
    let reader_role = match existing.as_deref() {
        Some(bytes) if bytes == base.as_slice() => Some(v2::DesktopArtifactRole::Base),
        Some(bytes) if bytes == target.as_slice() => Some(v2::DesktopArtifactRole::Target),
        Some(_) => anyhow::bail!("foreign reader cannot be adopted"),
        None => None,
    };
    let configured = retained_bytes(&root, "data")?;
    ensure!(
        configured
            .as_deref()
            .is_none_or(|b| b == b"theseus fixture data\n"),
        "foreign configured data cannot be adopted"
    );
    let legacy = manifest.desktop_definitions.is_empty();
    let selected = if args[0] == "uninstall" {
        None
    } else {
        let role = sut_definition_key(
            &args[0],
            if entry_only && args[0].starts_with("install_") {
                None
            } else {
                reader_role
            },
            if entry_only && args[0].starts_with("install_") {
                false
            } else {
                configured.is_some()
            },
        )?;
        // A combined SUT has already installed the shared reader exactly once.
        let role = if entry_only && args[0] == "install_target" && configured.is_some() {
            v2::DesktopDefinitionKey::CONFIGURED_TARGET
        } else {
            role
        };
        if legacy {
            None
        } else {
            Some(manifest.desktop_definition(&role)?)
        }
    };
    let selected_executable = selected.as_ref().map(|d| &d.executable).unwrap_or(
        manifest
            .desktop_executable
            .as_ref()
            .context("desktop executable")?,
    );
    let bytes = entry_bytes_for(selected_executable)?;
    let expected = selected.as_ref().map(|d| &d.desktop).unwrap_or(desktop);
    ensure!(
        format!("{:x}", Sha256::digest(&bytes)) == expected.entry_digest,
        "independent original entry commitment"
    );
    let mut allowed_entries = vec![entry_bytes(&manifest)?];
    for d in &manifest.desktop_definitions {
        allowed_entries.push(entry_bytes_for(&d.executable)?);
    }
    match args[0].as_str() {
        "install_base" | "install_target" => {
            let source = if args[0] == "install_base" {
                &args[2]
            } else {
                &args[3]
            };
            let image = if args[0] == "install_base" {
                &base
            } else {
                &target
            };
            let role = if args[0] == "install_base" {
                "base"
            } else {
                "target"
            };
            ensure!(
                config["artifacts"][role]["length"].as_u64() == Some(image.len() as u64)
                    && config["artifacts"][role]["sha256"].as_str()
                        == Some(format!("{:x}", Sha256::digest(image)).as_str()),
                "original image commitment"
            );
            if !entry_only {
                if existing.is_some() {
                    ensure!(
                        args[0] == "install_target"
                            && !legacy
                            && configured.is_some()
                            && retained_bytes(&root, "app")?.as_deref()
                                == Some(b"theseus fixture app\n"),
                        "replacement requires original configured app/data generation"
                    );
                    if args[4] != "missing_entry" {
                        ensure!(
                            allowed_entries
                                .iter()
                                .any(|b| std::fs::read(&entry).as_ref().ok() == Some(b)),
                            "original entry changed before replacement"
                        );
                    }
                    remove_exact(&reader, &[base.clone(), target.clone()])?;
                } else {
                    ensure!(
                        !entry.try_exists()? && retained_bytes(&root, "app")?.is_none(),
                        "preexisting reader/entry cannot be adopted"
                    );
                }
                std::fs::copy(source, &reader)?;
                if existing.is_none() {
                    root.publish(std::ffi::OsStr::new("app"), &mut |f| {
                        f.write_all(b"theseus fixture app\n")
                    })?;
                }
            } else {
                ensure!(
                    retained_bytes(&root, "reader")?.as_deref() == Some(image.as_slice()),
                    "combined shared reader was not installed by original SUT"
                );
            }
            if args[4] != "missing_entry" {
                entries.publish(std::ffi::OsStr::new(&desktop.entry), &mut |f| {
                    f.write_all(&bytes)
                })?;
            }
        }
        "configure" => {
            ensure!(
                reader_role.is_some(),
                "Configure requires original installed reader"
            );
            if args[4] != "missing_entry" {
                ensure!(
                    allowed_entries
                        .iter()
                        .any(|b| std::fs::read(&entry).as_ref().ok() == Some(b)),
                    "original entry changed before Configure"
                );
                entries.publish(std::ffi::OsStr::new(&desktop.entry), &mut |f| {
                    f.write_all(&bytes)
                })?;
            }
            if !entry_only {
                root.publish(std::ffi::OsStr::new("data"), &mut |f| {
                    f.write_all(b"theseus fixture data\n")
                })?;
            } else {
                ensure!(
                    configured.as_deref() == Some(b"theseus fixture data\n"),
                    "combined original SUT Configure data absent"
                );
            }
        }
        "uninstall" => {
            if args[4] == "leaked_entry" {
                return Ok(());
            }
            remove_exact(&entry, &allowed_entries)?;
            if !entry_only {
                remove_exact(&reader, &[base, target])?;
                remove_exact(&workspace.join("app"), &[b"theseus fixture app\n".to_vec()])?;
                remove_exact(
                    &workspace.join("data"),
                    &[b"theseus fixture data\n".to_vec()],
                )?;
            }
        }
        _ => anyhow::bail!("unsupported desktop fixture SUT action"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_sut_generation_key_keeps_original_artifact_and_same_tuple_reinstall() {
        use v2::{DesktopArtifactRole as A, DesktopDefinitionKey as K, DesktopDefinitionRole as R};
        assert_eq!(
            sut_definition_key("install_base", None, false).unwrap(),
            K {
                role: R::Base,
                artifact: A::Base
            }
        );
        assert_eq!(
            sut_definition_key("install_target", None, false).unwrap(),
            K {
                role: R::Target,
                artifact: A::Target
            }
        );
        assert_eq!(
            sut_definition_key("configure", Some(A::Base), true).unwrap(),
            K {
                role: R::Configured,
                artifact: A::Base
            }
        );
        assert_eq!(
            sut_definition_key("install_target", Some(A::Base), true).unwrap(),
            K::CONFIGURED_TARGET
        );
        assert_eq!(
            sut_definition_key("install_target", Some(A::Target), true).unwrap(),
            K::CONFIGURED_TARGET
        );
        assert!(sut_definition_key("configure", None, false).is_err());
        assert!(sut_definition_key("install_base", Some(A::Target), true).is_err());
    }
}
