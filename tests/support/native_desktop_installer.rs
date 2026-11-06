//! Compiled desktop SUT effects. The helper and broker never write entries.
use anyhow::{Context, Result, ensure};
use sha2::{Digest as _, Sha256};
use std::{
    io::{Read, Write},
    path::Path,
};
use theseus_host::fs::{HostDir, LeafOpenResult, RootPolicy, fingerprint};
use theseus_native_protocol::v2;
#[path = "native_desktop_entry.rs"]
mod installed_entry;
pub use installed_entry::metadata as native_entry_bytes;
pub fn entry_bytes(manifest: &v2::IntegrationManifest) -> Result<Vec<u8>> {
    declared_entry_bytes(
        manifest
            .desktop_executable
            .as_ref()
            .context("desktop executable missing")?,
        manifest.desktop_owned_entry.as_ref(),
    )
}
pub fn current_os() -> v2::Os {
    if cfg!(target_os = "macos") {
        v2::Os::Macos
    } else if cfg!(windows) {
        v2::Os::Windows
    } else {
        v2::Os::Linux
    }
}
pub fn os_name(os: v2::Os) -> &'static str {
    match os {
        v2::Os::Linux => "linux",
        v2::Os::Macos => "macos",
        v2::Os::Windows => "windows",
    }
}
fn declared_entry_bytes(
    x: &v2::DesktopExecutable,
    entry: Option<&v2::OwnedDesktopEntryDefinition>,
) -> Result<Vec<u8>> {
    match entry {
        Some(entry) => native_entry_bytes(entry, x),
        None => entry_bytes_for(x),
    }
}
fn entry_bytes_for(executable: &v2::DesktopExecutable) -> Result<Vec<u8>> {
    source_entry_bytes_for_platform(executable, current_os())
}
fn source_entry_bytes_for_platform(
    executable: &v2::DesktopExecutable,
    os: v2::Os,
) -> Result<Vec<u8>> {
    if os != v2::Os::Linux {
        ensure!(
            !executable.arguments.is_empty()
                && executable.arguments.len() <= 16
                && executable
                    .arguments
                    .iter()
                    .all(|s| s.len() <= 4096 && !s.contains('\0')),
            "bounded source argv before serialization"
        );
        let bytes = serde_json::to_vec(
            &serde_json::json!({"kind":"theseus_source_owned_entry","schema":1,"os":os_name(os),"argv":executable.arguments}),
        )?;
        ensure!(
            !bytes.is_empty()
                && bytes.len() <= 65536
                && executable.arguments.iter().all(|s| !s.contains('\0')),
            "bounded literal SourceOwned entry"
        );
        return Ok(bytes);
    }
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
    manifest.validate_context(&context, current_os())?;
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
    let reader_leaf = if cfg!(windows) {
        "reader.exe"
    } else {
        "reader"
    };
    ensure!(
        manifest.desktop_owned_entry.is_some()
            || manifest
                .desktop_executable
                .as_ref()
                .context("original executable")?
                .relative_path
                == reader_leaf,
        "exact platform reader leaf"
    );
    let reader = workspace.join(reader_leaf);
    let installed = installed_entry::Layout::new(&manifest)?;
    let entry = installed
        .as_ref()
        .map(|layout| layout.entry().to_owned())
        .unwrap_or_else(|| Path::new(&desktop.entry_root).join(&desktop.entry));
    let base = std::fs::read(&args[2])?;
    let target = std::fs::read(&args[3])?;
    let existing = retained_bytes(&root, reader_leaf)?;
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
    let bytes = declared_entry_bytes(selected_executable, manifest.desktop_owned_entry.as_ref())?;
    let expected = selected.as_ref().map(|d| &d.desktop).unwrap_or(desktop);
    ensure!(
        format!("{:x}", Sha256::digest(&bytes)) == expected.entry_digest,
        "independent original entry commitment"
    );
    let mut allowed_entries = vec![entry_bytes(&manifest)?];
    for d in &manifest.desktop_definitions {
        allowed_entries.push(declared_entry_bytes(&d.executable, d.owned_entry.as_ref())?);
    }
    if let Some(layout) = &installed {
        ensure!(
            !entry_only && args[4] == "none",
            "native entry fixture has no fault/combined mode"
        );
        layout.check_before(existing.as_deref(), &allowed_entries)?;
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
                    retained_bytes(&root, reader_leaf)?.as_deref() == Some(image.as_slice()),
                    "combined shared reader was not installed by original SUT"
                );
            }
            if let Some(layout) = &installed {
                layout.install(image, &bytes, existing.is_none())?;
            } else if args[4] != "missing_entry" {
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
                // Legacy Clean already admitted this exact singleton entry at
                // the installed checkpoint. Keep its original file/mapping;
                // only a keyed SUT generation may replace committed metadata.
                if let Some(layout) = &installed {
                    layout.configure(&bytes)?;
                } else if !legacy {
                    entries.publish(std::ffi::OsStr::new(&desktop.entry), &mut |f| {
                        f.write_all(&bytes)
                    })?;
                }
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
            if let Some(layout) = &installed {
                layout.uninstall(&[base.clone(), target.clone()], &allowed_entries)?;
            } else {
                remove_exact(&entry, &allowed_entries)?;
            }
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
    fn source_owned_entry_documents_keep_exact_platform_argv_without_native_claims() {
        let executable = v2::DesktopExecutable {
            root: r"C:\private fixture".into(),
            relative_path: "reader.exe".into(),
            length: 1,
            sha256: "11".repeat(32),
            arguments: vec![
                r"C:\private fixture\reader.exe".into(),
                "two words".into(),
                "quote\"and\\slash".into(),
            ],
            environment: vec![],
            working_directory: r"C:\private fixture".into(),
        };
        for (os, expected) in [(v2::Os::Macos, "macos"), (v2::Os::Windows, "windows")] {
            let bytes = source_entry_bytes_for_platform(&executable, os).unwrap();
            let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(document["kind"], "theseus_source_owned_entry");
            assert_eq!(document["os"], expected);
            assert_eq!(document["argv"], serde_json::json!(executable.arguments));
        }
        assert!(source_entry_bytes_for_platform(&executable, v2::Os::Linux).is_err());
        let mut large = executable;
        large.arguments = vec!["a".repeat(65536)];
        assert!(source_entry_bytes_for_platform(&large, v2::Os::Windows).is_err());
        large.arguments = vec!["a\0b".into()];
        assert!(source_entry_bytes_for_platform(&large, v2::Os::Macos).is_err());
    }
    fn configure_fixture(
        keyed: bool,
    ) -> (
        tempfile::TempDir,
        Vec<String>,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let temp = tempfile::Builder::new()
            .prefix("th-legacy-configure-")
            .tempdir()
            .unwrap();
        let workspace = temp.path().join("workspace");
        let entries = temp.path().join("entries");
        HostDir::create_private(&workspace).unwrap();
        HostDir::create_private(&entries).unwrap();
        let base = temp.path().join("base");
        let target = temp.path().join("target");
        std::fs::write(&base, b"base-image").unwrap();
        std::fs::write(&target, b"target-image").unwrap();
        let image_digest = format!("{:x}", Sha256::digest(b"target-image"));
        let data_digest = format!("{:x}", Sha256::digest(b"theseus fixture data\n"));
        let expectation = v2::ReadinessDefinitionExpectation {
            role: "configured".into(),
            artifact: "target".into(),
            artifact_length: 12,
            artifact_sha256: image_digest.clone(),
            version_literal: "target\n".into(),
            data_sha256: data_digest.clone(),
            configuration_digest: data_digest,
        };
        let executable = v2::DesktopExecutable {
            root: workspace.display().to_string(),
            relative_path: "reader".into(),
            length: 12,
            sha256: image_digest.clone(),
            arguments: vec![
                workspace.join("reader").display().to_string(),
                "configured-marker".into(),
            ],
            environment: vec![],
            working_directory: workspace.display().to_string(),
        };
        let bytes = entry_bytes_for(&executable).unwrap();
        let entry_digest = format!("{:x}", Sha256::digest(&bytes));
        let mut manifest:v2::IntegrationManifest = serde_json::from_value(serde_json::json!({"schema":1,"core_manifest_digest":"11".repeat(32),"desktop":{"schema":1,"core_manifest_digest":"11".repeat(32),"slot":"desktop","entry_root":entries,"namespace_policy":"exclusive_quiescent","entry":"fixture.desktop","entry_digest":entry_digest,"executable_digest":image_digest,"allowed_modes":["owned_executable"],"ready_endpoint":{"kind":"unix_path","path":"/fixture/ready.sock"},"close_endpoint":{"kind":"unix_path","path":"/fixture/close.sock"},"expectation_digest":expectation.digest().unwrap().to_hex()},"desktop_executable":executable,"desktop_readiness":expectation,"definitions":[],"artifact_slots":[{"slot":"desktop","relative_path":"fixture.desktop","length":bytes.len(),"sha256":entry_digest}],"readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":["inspect_desktop","owned_executable","request_close"]})).unwrap();
        if keyed {
            let mut initial = manifest
                .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
                .unwrap();
            initial.key = v2::DesktopDefinitionKey {
                role: v2::DesktopDefinitionRole::Target,
                artifact: v2::DesktopArtifactRole::Target,
            };
            initial.executable.arguments[1] = "installed-marker".into();
            initial.readiness.role = "target".into();
            initial.desktop.expectation_digest = initial.readiness.digest().unwrap().to_hex();
            let bytes = entry_bytes_for(&initial.executable).unwrap();
            initial.entry_length = bytes.len() as u64;
            initial.desktop.entry_digest = format!("{:x}", Sha256::digest(&bytes));
            manifest.desktop_definitions.push(initial);
        }
        manifest.validate().unwrap();
        let integration = manifest.digest().unwrap().to_hex();
        let full = v2::integration_manifest_digest(
            v2::Digest::parse(&manifest.core_manifest_digest).unwrap(),
            manifest.digest().unwrap(),
        )
        .to_hex();
        let header = serde_json::json!({"profile":"native_integration_broker_v1","binding":"01".repeat(16),"manifest_digest":full,"workspace_marker":"02".repeat(16),"os":"linux","target_id":"fixture","ownership_id":"03".repeat(16),"provider_epoch":"04".repeat(32),"session_id":"fixture-session","session_epoch":"05".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},"allowed_modes":["owned_executable"]});
        let context = serde_json::json!({"kind":"theseus_native_integration_context","schema":1,"binding":"01".repeat(16),"manifest_digest":full,"integration_manifest_digest":integration,"workspace_marker":"02".repeat(16),"ownership_id":"03".repeat(16),"provider_epoch":"04".repeat(32),"desktop_broker_header":header});
        let config = temp.path().join("config.json");
        let config_value = serde_json::json!({"integration_manifest":manifest,"runtime_context":context,"workspace_root":workspace,"artifacts":{"base":{"length":10,"sha256":format!("{:x}",Sha256::digest(b"base-image"))},"target":{"length":12,"sha256":image_digest}}});
        std::fs::write(&config, serde_json::to_vec(&config_value).unwrap()).unwrap();
        let args = vec![
            "install_target".into(),
            config.display().to_string(),
            base.display().to_string(),
            target.display().to_string(),
            "none".into(),
        ];
        run(&args).unwrap();
        (
            temp,
            args,
            entries.join("fixture.desktop"),
            workspace.join("reader"),
        )
    }

    #[test]
    fn legacy_configure_preserves_original_entry_and_reader_mappings() {
        let (_temp, mut args, entry, reader) = configure_fixture(false);
        let held_entry = std::fs::File::open(&entry).unwrap();
        let held_reader = std::fs::File::open(&reader).unwrap();
        let original_entry = fingerprint(&held_entry).unwrap();
        let original_reader = fingerprint(&held_reader).unwrap();
        let original_bytes = std::fs::read(&entry).unwrap();
        args[0] = "configure".into();
        run(&args).unwrap();
        assert_eq!(
            fingerprint(&std::fs::File::open(&entry).unwrap()).unwrap(),
            original_entry,
            "legacy Configure replaced the original admitted entry mapping"
        );
        assert_eq!(
            fingerprint(&held_entry).unwrap(),
            original_entry,
            "legacy Configure changed the original held entry"
        );
        assert_eq!(
            fingerprint(&std::fs::File::open(&reader).unwrap()).unwrap(),
            original_reader
        );
        assert_eq!(fingerprint(&held_reader).unwrap(), original_reader);
        assert_eq!(std::fs::read(&entry).unwrap(), original_bytes);
        assert_eq!(
            std::fs::read(reader.parent().unwrap().join("data")).unwrap(),
            b"theseus fixture data\n"
        );
    }

    #[test]
    fn keyed_configure_publishes_only_its_committed_configured_entry() {
        let (_temp, mut args, entry, reader) = configure_fixture(true);
        let before = std::fs::read(&entry).unwrap();
        let reader_before = fingerprint(&std::fs::File::open(&reader).unwrap()).unwrap();
        args[0] = "configure".into();
        run(&args).unwrap();
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&args[1]).unwrap()).unwrap();
        let manifest: v2::IntegrationManifest =
            serde_json::from_value(config["integration_manifest"].clone()).unwrap();
        let after = std::fs::read(&entry).unwrap();
        assert_ne!(before, after);
        assert_eq!(
            format!("{:x}", Sha256::digest(&after)),
            manifest.desktop.as_ref().unwrap().entry_digest
        );
        assert_eq!(
            fingerprint(&std::fs::File::open(&reader).unwrap()).unwrap(),
            reader_before
        );
        assert_eq!(
            std::fs::read(reader.parent().unwrap().join("data")).unwrap(),
            b"theseus fixture data\n"
        );
    }

    #[test]
    fn legacy_configure_refuses_foreign_entry_instead_of_adopting_it() {
        let (_temp, mut args, entry, reader) = configure_fixture(false);
        std::fs::write(&entry, b"foreign entry").unwrap();
        args[0] = "configure".into();
        assert!(run(&args).is_err());
        assert_eq!(std::fs::read(entry).unwrap(), b"foreign entry");
        assert!(!reader.parent().unwrap().join("data").exists());
    }

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
