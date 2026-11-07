//! Owned entry parser admission is independent of native activation and lifetime.
use std::io;
use theseus_host::desktop::session::{BoundSession, DesktopKind, DesktopMode};
use theseus_native_protocol::v2::OwnedDesktopEntryDefinition;
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}
pub(crate) fn validate_platform(
    entry: Option<&OwnedDesktopEntryDefinition>,
    os: &str,
) -> io::Result<()> {
    if let Some(entry) = entry
        && (entry.os() != os || entry.os() != std::env::consts::OS)
    {
        return Err(invalid(
            "owned entry requires its actual compiled platform parser",
        ));
    }
    Ok(())
}
pub(super) fn validate_session(
    entry: Option<&OwnedDesktopEntryDefinition>,
    session: &BoundSession,
    os: &str,
) -> io::Result<()> {
    validate_platform(entry, os)?;
    if let Some(entry) = entry {
        if !matches!(
            (entry.os(), session.kind()),
            ("macos", DesktopKind::MacOs) | ("windows", DesktopKind::Windows)
        ) {
            return Err(invalid(
                "owned entry differs from original supplied session backend",
            ));
        }
        session.authorize(DesktopMode::OwnedExecutable)?;
    }
    Ok(())
}
#[cfg(any(not(target_os = "linux"), test))]
pub(super) fn require_interactive_definition(
    native: bool,
    owned: bool,
    source: bool,
) -> io::Result<()> {
    if native && owned || !native && !owned && !source {
        return Err(invalid(
            "interactive non-Linux owned entry requires exact platform parser declaration; unqualified JSON is source-only",
        ));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use theseus_native_protocol::v2::{
        NativeArtifactCommitment, OwnedDesktopEntryDefinition as Entry,
    };
    fn mac() -> Entry {
        Entry::MacosBundle {
            app_leaf: "Fixture.app".into(),
            bundle_identifier: "org.theseus.fixture".into(),
            executable_leaf: "app".into(),
            info_plist: NativeArtifactCommitment {
                length: 128,
                sha256: "22".repeat(32),
            },
        }
    }
    #[test]
    fn owned_entry_requires_the_actual_compiled_parser_not_a_declared_platform() {
        assert_eq!(
            validate_platform(Some(&mac()), "macos").is_ok(),
            cfg!(target_os = "macos")
        );
        assert_eq!(
            validate_platform(Some(&Entry::WindowsShortcut), "windows").is_ok(),
            cfg!(windows)
        );
        assert!(validate_platform(Some(&mac()), "windows").is_err());
        assert!(validate_platform(Some(&Entry::WindowsShortcut), "linux").is_err());
        assert!(validate_platform(None, "linux").is_ok());
    }
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn owned_entry_keeps_original_owned_authorization_and_refuses_revocation_or_native_only() {
        use theseus_host::{
            desktop::session::{BrokerEndpoint, SessionAuthority, SessionBinding, SuppliedDisplay},
            principal::CurrentPrincipal,
            services::readiness::{NamedPipe, UnixPath},
        };
        let (kind, entry, endpoint, display, principal) = if cfg!(target_os = "macos") {
            (
                DesktopKind::MacOs,
                mac(),
                BrokerEndpoint::Unix(UnixPath::new("/fixture/broker.sock".into()).unwrap()),
                SuppliedDisplay::MacOsGui {
                    domain: "gui/501".into(),
                },
                CurrentPrincipal::Posix {
                    effective_uid: 501,
                    effective_gid: 20,
                },
            )
        } else {
            (
                DesktopKind::Windows,
                Entry::WindowsShortcut,
                BrokerEndpoint::Pipe(NamedPipe::new(r"\\.\pipe\theseus.broker".into()).unwrap()),
                SuppliedDisplay::WindowsInteractive {
                    session_id: 1,
                    window_station: "WinSta0".into(),
                    desktop: "Default".into(),
                },
                CurrentPrincipal::Windows {
                    token_user_sid: "S-1-5-21-1000".into(),
                },
            )
        };
        let binding = SessionBinding {
            target_id: "fixture".into(),
            ownership_id: [1; 16],
            provider_epoch: [2; 32],
            session_id: "supplied".into(),
            session_epoch: [3; 32],
            broker_header_digest: [4; 32],
            manifest_digest: [5; 32],
            broker_endpoint: endpoint,
            broker_principal: principal.clone(),
            helper_principal: principal,
            display,
        };
        let bound = |modes| {
            BoundSession::admit(
                SessionAuthority {
                    kind,
                    binding: binding.clone(),
                    allowed_modes: modes,
                },
                &binding,
            )
            .unwrap()
        };
        let mut original = bound(vec![DesktopMode::OwnedExecutable]);
        validate_session(Some(&entry), &original, entry.os()).unwrap();
        assert!(original.authorize(DesktopMode::NativeActivation).is_err());
        assert!(original.observe(None).is_err());
        assert!(validate_session(Some(&entry), &original, entry.os()).is_err());
        let native_only = bound(vec![DesktopMode::NativeActivation]);
        assert!(validate_session(Some(&entry), &native_only, entry.os()).is_err());
    }
    #[test]
    fn interactive_entry_gate_keeps_unqualified_json_in_source_only_profile() {
        assert!(require_interactive_definition(false, false, false).is_err());
        assert!(require_interactive_definition(false, false, true).is_ok());
        assert!(require_interactive_definition(false, true, false).is_ok());
        assert!(require_interactive_definition(true, false, false).is_ok());
        assert!(require_interactive_definition(true, true, false).is_err());
    }
}

#[cfg(all(test, unix))]
mod observed_files_tests {
    use super::super::{BrokerServerConfig, launch};
    use std::fs;
    use theseus_native_protocol::v2;
    #[test]
    fn owned_bundle_directory_is_present_even_when_installed_metadata_is_missing() {
        // Read-only observation model, never an admitted Mac parser/backend.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        let mut raw = super::super::native_routing_tests::native_config();
        raw["manifest"]
            .as_object_mut()
            .unwrap()
            .remove("desktop_native");
        raw["manifest"]["desktop_owned_entry"] = serde_json::json!({"kind":"macos_bundle","app_leaf":"Fixture.app","bundle_identifier":"org.theseus.fixture","executable_leaf":"app","info_plist":{"length":128,"sha256":"33".repeat(32)}});
        raw["manifest"]["desktop"]["entry_root"] = root.into();
        raw["manifest"]["desktop"]["entry"] = "Fixture.app".into();
        raw["manifest"]["desktop"]["allowed_modes"] = serde_json::json!(["owned_executable"]);
        raw["manifest"]["desktop_executable"]["root"] = root.into();
        raw["manifest"]["desktop_executable"]["relative_path"] =
            "Fixture.app/Contents/MacOS/app".into();
        raw["manifest"]["desktop_executable"]["arguments"] =
            serde_json::json!([format!("{root}/Fixture.app/Contents/MacOS/app")]);
        raw["manifest"]["desktop_executable"]["working_directory"] = root.into();
        raw["manifest"]["desktop_capabilities"] =
            serde_json::json!(["inspect_desktop", "owned_executable", "request_close"]);
        raw["manifest"]["artifact_slots"][0]["relative_path"] =
            "Fixture.app/Contents/Info.plist".into();
        let m: v2::IntegrationManifest = serde_json::from_value(raw["manifest"].clone()).unwrap();
        m.validate().unwrap();
        let config: BrokerServerConfig = serde_json::from_value(raw).unwrap();
        let tuple = m
            .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
            .unwrap();
        let roots = launch::DefinitionRoots::open(&config).unwrap();
        let absent = launch::inspect(&config, &roots, &tuple).unwrap();
        assert!(!absent.entry_present);
        fs::create_dir(dir.path().join("Fixture.app")).unwrap();
        let leaked = launch::inspect(&config, &roots, &tuple).unwrap();
        assert!(leaked.entry_present);
        assert!(leaked.entry_digest.is_none());
        assert!(leaked.executable_digest.is_none());
    }
}
