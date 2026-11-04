use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

const GOOD: &[u8] = b"[Desktop Entry]\nType=Application\nName=Theseus fixture\nTerminal=false\nDBusActivatable=false\nExec=/fixture/app --ready literal\n";

// Catches accepting launcher-policy changes or interpreting caller inputs.
#[test]
fn linux_desktop_parser_compares_exact_admitted_literal_command() {
    let expected = LiteralCommand {
        executable: "/fixture/app".into(),
        arguments: vec!["--ready".into(), "literal".into()],
    };
    assert!(parse_entry(GOOD, &expected).is_ok());
    let wrong = LiteralCommand {
        arguments: vec!["--different".into()],
        ..expected.clone()
    };
    assert!(parse_entry(GOOD, &wrong).is_err());
    // Even an independently supplied matching argv cannot widen this backend's
    // closed no-URI/field-code/shell capability.
    for argument in [
        "file:/tmp/data",
        "mailto:user",
        "file:///tmp/data",
        "%U",
        "literal;echo",
        "$(echo)",
    ] {
        let unsafe_expected = LiteralCommand {
            executable: "/fixture/app".into(),
            arguments: vec!["--ready".into(), argument.into()],
        };
        let entry = String::from_utf8(GOOD.to_vec())
            .unwrap()
            .replace("literal", argument);
        assert!(
            parse_entry(entry.as_bytes(), &unsafe_expected).is_err(),
            "accepted admitted unsafe argv {argument}"
        );
    }
    for (from, to) in [
        ("Terminal=false", "Terminal=true"),
        ("DBusActivatable=false", "DBusActivatable=true"),
        ("Type=Application", "Type=Link"),
        ("literal", "%U"),
        ("literal", "file:///tmp/data"),
        ("literal", "file:/tmp/data"),
        ("literal", "mailto:user"),
        ("/fixture/app", "app"),
        ("literal", "literal;echo"),
        ("literal", "$(echo)"),
        ("literal", "literal & /other"),
    ] {
        let entry = String::from_utf8(GOOD.to_vec()).unwrap().replace(from, to);
        assert!(
            parse_entry(entry.as_bytes(), &expected).is_err(),
            "accepted {to}"
        );
    }
    for extra in [
        "TryExec=/fixture/app\n",
        "Path=/tmp\n",
        "Terminal=false\n",
        "Actions=other;\n",
        "[Desktop Action other]\nExec=/other\n",
    ] {
        let mut entry = GOOD.to_vec();
        entry.extend_from_slice(extra.as_bytes());
        assert!(parse_entry(&entry, &expected).is_err(), "accepted {extra}");
    }
    assert!(parse_entry(&vec![b'x'; 65537], &expected).is_err());
}

fn definition(root: &Path) -> AdmittedDefinition {
    AdmittedDefinition {
        entry: ArtifactSlot {
            root: root.to_path_buf(),
            relative: "fixture.desktop".into(),
            length: GOOD.len() as u64,
            sha256: hash_bytes(GOOD),
        },
        executable: ArtifactSlot {
            root: root.to_path_buf(),
            relative: "app".into(),
            length: 3,
            sha256: [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ],
        },
        command: LiteralCommand {
            executable: root.join("app").to_str().unwrap().into(),
            arguments: vec!["--ready".into(), "literal".into()],
        },
    }
}
fn fixture() -> (tempfile::TempDir, AdmittedDefinition) {
    let temp = tempfile::tempdir().unwrap();
    let mut d = definition(temp.path());
    let bytes = String::from_utf8(GOOD.to_vec())
        .unwrap()
        .replace("/fixture/app", &d.command.executable)
        .into_bytes();
    d.entry.length = bytes.len() as u64;
    d.entry.sha256 = hash_bytes(&bytes);
    fs::write(temp.path().join("fixture.desktop"), bytes).unwrap();
    fs::write(temp.path().join("app"), b"abc").unwrap();
    fs::set_permissions(temp.path().join("app"), fs::Permissions::from_mode(0o700)).unwrap();
    (temp, d)
}
// Catches trusting entry/self-reported expected hashes instead of supplied commitments.
#[test]
fn linux_desktop_retained_files_pin_supplied_lengths_hashes_and_mapping() {
    let (temp, d) = fixture();
    let retained = RetainedEntry::admit(d.clone()).unwrap();
    assert!(retained.revalidate().is_ok());
    let mut wrong = d.clone();
    wrong.executable.sha256 = [1; 32];
    assert!(RetainedEntry::admit(wrong).is_err());
    let mut wrong = d.clone();
    wrong.entry.length += 1;
    assert!(RetainedEntry::admit(wrong).is_err());
    fs::rename(temp.path().join("app"), temp.path().join("old")).unwrap();
    fs::write(temp.path().join("app"), b"abc").unwrap();
    fs::set_permissions(temp.path().join("app"), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(retained.revalidate().is_err());
}
// Catches following aliases or opening attacker-selected entry/executable roots.
#[test]
fn linux_desktop_rejects_symlinks_and_traversal_without_writing_entries() {
    let (temp, d) = fixture();
    fs::rename(
        temp.path().join("fixture.desktop"),
        temp.path().join("real.desktop"),
    )
    .unwrap();
    symlink("real.desktop", temp.path().join("fixture.desktop")).unwrap();
    assert!(RetainedEntry::admit(d.clone()).is_err());
    let mut traversal = d.clone();
    traversal.entry.relative = "../real.desktop".into();
    assert!(RetainedEntry::admit(traversal).is_err());
    fs::remove_file(temp.path().join("fixture.desktop")).unwrap();
    assert!(RetainedEntry::admit(d).is_err());
    assert!(!temp.path().join("fixture.desktop").exists());
}
// Qualified original-child capability admits primary only; tree refuses before SDK effect.
#[test]
fn linux_native_lifetime_gate_admits_original_primary_and_refuses_tree() {
    assert!(validate_lifetime(NativeLifetimeRequirement::AcknowledgementOnly).is_ok());
    assert!(validate_lifetime(NativeLifetimeRequirement::OwnedPrimary).is_ok());
    assert!(validate_lifetime(NativeLifetimeRequirement::OwnedTree).is_err());
}

fn session() -> crate::desktop::session::BoundSession {
    use crate::desktop::session::*;
    use crate::principal::CurrentPrincipal;
    use crate::services::readiness::UnixPath;
    let b = SessionBinding {
        target_id: "fixture".into(),
        ownership_id: [1; 16],
        provider_epoch: [2; 32],
        session_id: "private-x".into(),
        session_epoch: [3; 32],
        broker_header_digest: [4; 32],
        manifest_digest: [5; 32],
        broker_endpoint: BrokerEndpoint::Unix(
            UnixPath::new("/private/broker.sock".into()).unwrap(),
        ),
        broker_principal: CurrentPrincipal::Posix {
            effective_uid: 1000,
            effective_gid: 1000,
        },
        helper_principal: CurrentPrincipal::Posix {
            effective_uid: 1000,
            effective_gid: 1000,
        },
        display: SuppliedDisplay::X11 {
            display: ":91".into(),
            authority_file: "/private/auth".into(),
            bus: UnixPath::new("/private/bus".into()).unwrap(),
        },
    };
    BoundSession::admit(
        SessionAuthority {
            kind: DesktopKind::Linux,
            binding: b.clone(),
            allowed_modes: vec![DesktopMode::NativeActivation],
        },
        &b,
    )
    .unwrap()
}
// Catches inherited display/session, duplicate overrides and dynamic-loader injection.
#[test]
fn linux_native_environment_requires_exact_supplied_display_bus_and_closed_keys() {
    let s = session();
    let env = vec![
        ("DISPLAY".into(), ":91".into()),
        ("XAUTHORITY".into(), "/private/auth".into()),
        (
            "DBUS_SESSION_BUS_ADDRESS".into(),
            "unix:path=/private/bus".into(),
        ),
        (
            "GIO_LAUNCH_DESKTOP".into(),
            "/sdk/gio-launch-desktop".into(),
        ),
    ];
    assert!(validate_environment(&s, &env, "/sdk/gio-launch-desktop").is_ok());
    for bad in [
        ("DISPLAY", ":0"),
        ("DBUS_SESSION_BUS_ADDRESS", "autolaunch:"),
        ("WAYLAND_DISPLAY", "wayland-0"),
        ("LD_PRELOAD", "/tmp/inject.so"),
        ("PATH", "/usr/bin"),
        ("GIO_EXTRA_MODULES", "/tmp"),
        ("GIO_LAUNCH_DESKTOP", "helper"),
    ] {
        let mut changed = env.clone();
        changed.retain(|(k, _)| k != bad.0);
        changed.push((bad.0.into(), bad.1.into()));
        assert!(
            validate_environment(&s, &changed, "/sdk/gio-launch-desktop").is_err(),
            "accepted {bad:?}"
        );
    }
    let mut duplicate = env.clone();
    duplicate.push(("DISPLAY".into(), ":91".into()));
    assert!(validate_environment(&s, &duplicate, "/sdk/gio-launch-desktop").is_err());
    let mut missing = env.clone();
    missing.retain(|(k, _)| k != "DBUS_SESSION_BUS_ADDRESS");
    assert!(validate_environment(&s, &missing, "/sdk/gio-launch-desktop").is_err());
}

// Catches promoting SDK success or a numeric callback to readiness/cleanup rights.
#[test]
fn linux_native_callback_receipt_keeps_acknowledgement_instance_and_lifetime_distinct() {
    let mut callbacks = NativeCallbacks::default();
    assert_eq!(callbacks.receipt(true), NativeLaunchReceipt::Ambiguous);
    callbacks.record(123);
    assert_eq!(
        callbacks.receipt(true),
        NativeLaunchReceipt::AcknowledgedSpawnCallback {
            diagnostic_pid: 123
        }
    );
    assert_eq!(
        callbacks.receipt(false),
        NativeLaunchReceipt::FailedOrPartial
    );
    callbacks.record(124);
    assert_eq!(callbacks.receipt(true), NativeLaunchReceipt::Ambiguous);
}

fn native_elf_fixture() -> Vec<u8> {
    let mut elf = vec![0u8; 128];
    elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    elf[16..18].copy_from_slice(&2u16.to_le_bytes());
    let machine = if cfg!(target_arch = "aarch64") {
        183u16
    } else {
        62u16
    };
    elf[18..20].copy_from_slice(&machine.to_le_bytes());
    elf[20..24].copy_from_slice(&1u32.to_le_bytes());
    elf[24..32].copy_from_slice(&0x400078u64.to_le_bytes());
    elf[32..40].copy_from_slice(&64u64.to_le_bytes());
    elf[52..54].copy_from_slice(&64u16.to_le_bytes());
    elf[54..56].copy_from_slice(&56u16.to_le_bytes());
    elf[56..58].copy_from_slice(&1u16.to_le_bytes());
    elf[64..68].copy_from_slice(&1u32.to_le_bytes());
    elf[68..72].copy_from_slice(&5u32.to_le_bytes());
    elf[80..88].copy_from_slice(&0x400000u64.to_le_bytes());
    elf[96..104].copy_from_slice(&128u64.to_le_bytes());
    elf[104..112].copy_from_slice(&128u64.to_le_bytes());
    elf[112..120].copy_from_slice(&4096u64.to_le_bytes());
    elf
}
// Catches admitting merely hashed/script/foreign/malformed bytes to GIO's
// implicit ENOEXEC shell fallback. These tests never load the native SDK.
#[test]
fn linux_native_binary_requires_independent_qualification_and_strict_host_elf() {
    let temp = tempfile::tempdir().unwrap();
    let bytes = native_elf_fixture();
    let path = temp.path().join("binary");
    fs::write(&path, &bytes).unwrap();
    let file = std::fs::File::open(&path).unwrap();
    let slot = ArtifactSlot {
        root: temp.path().to_owned(),
        relative: "binary".into(),
        length: bytes.len() as u64,
        sha256: hash_bytes(&bytes),
    };
    let abi = NativeElfAbi::current().unwrap();
    let sdk = QualifiedGioSdk {
        glib_version: [2, 90, 1],
        native_abi: abi,
        helper_length: slot.length,
        helper_sha256: slot.sha256,
        qualification_digest: [11; 32],
    };
    let qualification = CompiledArtifactQualification {
        length: slot.length,
        sha256: slot.sha256,
        native_abi: abi,
        sdk_qualification_digest: [11; 32],
        provenance: QualificationProvenance::ProviderBuildAttested {
            verified_receipt_digest: [12; 32],
        },
    };
    assert_eq!(
        validate_compiled_binary(&file, &slot, None, &sdk)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Unsupported
    );
    let admitted = validate_compiled_binary(&file, &slot, Some(&qualification), &sdk).unwrap();
    assert_eq!(admitted.observed.sha256, slot.sha256);
    assert_eq!(admitted.qualification.provenance, qualification.provenance);
    let mut changed = qualification.clone();
    changed.length += 1;
    assert!(validate_compiled_binary(&file, &slot, Some(&changed), &sdk).is_err());
    let mut wrong = qualification.clone();
    wrong.sha256 = [99; 32];
    assert!(validate_compiled_binary(&file, &slot, Some(&wrong), &sdk).is_err());
    let mut wrong = qualification.clone();
    wrong.sdk_qualification_digest = [99; 32];
    assert!(validate_compiled_binary(&file, &slot, Some(&wrong), &sdk).is_err());
    let mut wrong = qualification.clone();
    wrong.native_abi = if abi == NativeElfAbi::LinuxX86_64 {
        NativeElfAbi::LinuxAarch64
    } else {
        NativeElfAbi::LinuxX86_64
    };
    assert!(validate_compiled_binary(&file, &slot, Some(&wrong), &sdk).is_err());
    let mut wrong = qualification.clone();
    wrong.provenance = QualificationProvenance::ProviderBuildAttested {
        verified_receipt_digest: [0; 32],
    };
    assert!(validate_compiled_binary(&file, &slot, Some(&wrong), &sdk).is_err());
    for mutation in [0usize, 4, 5, 18, 52, 54, 56, 64, 96, 104] {
        let mut malformed = bytes.clone();
        malformed[mutation] = if mutation == 104 { 0 } else { 0xff };
        fs::write(&path, &malformed).unwrap();
        let mut malformed_slot = slot.clone();
        malformed_slot.sha256 = hash_bytes(&malformed);
        let mut malformed_q = qualification.clone();
        malformed_q.sha256 = malformed_slot.sha256;
        assert!(
            validate_compiled_binary(&file, &malformed_slot, Some(&malformed_q), &sdk).is_err(),
            "accepted ELF mutation {mutation}"
        );
    }
    fs::write(&path, b"#!/bin/sh\necho forbidden\n").unwrap();
    let mut script = slot.clone();
    script.length = 25;
    script.sha256 = hash_bytes(b"#!/bin/sh\necho forbidden\n");
    let mut script_q = qualification;
    script_q.length = script.length;
    script_q.sha256 = script.sha256;
    assert!(validate_compiled_binary(&file, &script, Some(&script_q), &sdk).is_err());
}

#[test]
fn linux_original_child_reserved_exit_reaps_once_after_explicit_ipc_retirement_model() {
    let invocation = OriginalInvocation { process: 17 };
    let mut child = OriginalGioChild::retain_callback(&invocation, 321);
    assert_eq!(
        child
            .poll_with(|pid| {
                assert_eq!(pid, 321);
                Ok(ChildWait::Running)
            })
            .unwrap(),
        NativePrimaryState::Running
    );
    assert!(
        !child
            .reap_with(|_| panic!("live child must not be reaped"))
            .unwrap()
    );
    assert_eq!(
        child
            .poll_with(|_| Ok(ChildWait::Exited {
                code: libc::CLD_EXITED,
                status: 7
            }))
            .unwrap(),
        NativePrimaryState::Exited
    );
    assert_eq!(
        child
            .poll_with(|_| Ok(ChildWait::Exited {
                code: libc::CLD_EXITED,
                status: 7
            }))
            .unwrap(),
        NativePrimaryState::Exited
    );
    let calls = std::cell::Cell::new(0);
    assert!(
        child
            .reap_with(|pid| {
                assert_eq!(pid, 321);
                calls.set(calls.get() + 1);
                Ok(Some(7 << 8))
            })
            .unwrap()
    );
    assert!(
        child
            .reap_with(|_| panic!("final reap must be once only"))
            .unwrap()
    );
    assert_eq!(calls.get(), 1);
    assert_eq!(child.state, ChildReservation::Reaped);
}
#[test]
fn linux_original_child_early_echild_is_sticky_unknown_never_reattached_or_reaped_model() {
    let invocation = OriginalInvocation { process: 17 };
    let mut child = OriginalGioChild::retain_callback(&invocation, 321);
    let error = child
        .poll_with(|_| Err(io::Error::from_raw_os_error(libc::ECHILD)))
        .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
    assert_eq!(child.state, ChildReservation::Unknown);
    assert_eq!(
        child
            .poll_with(|_| panic!("lost child identity cannot be retried"))
            .unwrap(),
        NativePrimaryState::Unknown
    );
    assert!(
        !child
            .reap_with(|_| panic!("unknown reservation grants no reap authority"))
            .unwrap()
    );
}
#[test]
fn linux_native_callback_storage_retains_multiple_partial_and_overflow_owners_model() {
    let mut callbacks = NativeCallbacks::default();
    callbacks.record(321);
    callbacks.record(322);
    assert_eq!(&callbacks.pids[..callbacks.stored], &[321, 322]);
    assert_eq!(callbacks.receipt(true), NativeLaunchReceipt::Ambiguous);
    assert_eq!(
        callbacks.receipt(false),
        NativeLaunchReceipt::FailedOrPartial
    );
    for index in 2..MAX_CALLBACKS + 3 {
        callbacks.record(400 + index as i32);
    }
    assert!(callbacks.overflow);
    assert_eq!(callbacks.stored, MAX_CALLBACKS);
    assert_eq!(&callbacks.pids[..2], &[321, 322]);
    assert_eq!(callbacks.receipt(true), NativeLaunchReceipt::Ambiguous);
    assert_eq!(sdk::spawn_flags(true) & 2, 2);
    assert_eq!(sdk::spawn_flags(false) & 2, 0);
    assert_eq!(sdk::spawn_flags(true) & 4, 0, "PATH search is forbidden");
}
#[test]
fn linux_original_native_retirement_requires_flush_close_and_no_late_or_error_callback_model() {
    use sdk::RetirementProgress as P;
    let mut progress = P::Flushing;
    progress.flush_complete(true);
    assert_eq!(progress, P::Flushed);
    progress = P::Closing;
    progress.close_complete(true);
    assert_eq!(progress, P::Closed);
    progress.close_complete(true);
    assert_eq!(
        progress,
        P::Unknown,
        "duplicate callback revokes settlement"
    );
    progress.flush_complete(true);
    assert_eq!(progress, P::Unknown);
    let mut failed = P::Flushing;
    failed.flush_complete(false);
    assert_eq!(failed, P::Unknown);
    failed.close_complete(true);
    assert_eq!(failed, P::Unknown);
    let mut wrong = P::Idle;
    wrong.close_complete(true);
    assert_eq!(wrong, P::Unknown);
}

#[test]
fn linux_original_reserved_exit_lost_before_ipc_remains_unknown_model() {
    let invocation = OriginalInvocation { process: 17 };
    let mut child = OriginalGioChild::retain_callback(&invocation, 321);
    child
        .poll_with(|_| {
            Ok(ChildWait::Exited {
                code: libc::CLD_EXITED,
                status: 0,
            })
        })
        .unwrap();
    assert!(
        child
            .poll_with(|_| Err(io::Error::from_raw_os_error(libc::ECHILD)))
            .is_err()
    );
    assert_eq!(child.state, ChildReservation::Unknown);
    assert!(
        !child
            .reap_with(|_| panic!("lost reserved exit cannot authorize PID reuse"))
            .unwrap()
    );
}

// Separate actual child/wait primitive source fixtures. They do not load GIO,
// create a native activation owner, infer any desktop session, or claim a tree.
#[cfg(feature = "desktop-test-support")]
#[test]
fn linux_original_wait_primitive_child_fixture() {
    if std::env::var_os("THESEUS_LINUX_WAIT_PRIMITIVE_CHILD").as_deref()
        == Some(std::ffi::OsStr::new("exit23"))
    {
        std::process::exit(23);
    }
}
#[cfg(feature = "desktop-test-support")]
fn original_wait_primitive_child() -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "desktop::linux::tests::linux_original_wait_primitive_child_fixture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("THESEUS_LINUX_WAIT_PRIMITIVE_CHILD", "exit23")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap()
}
#[cfg(feature = "desktop-test-support")]
#[test]
fn linux_original_wait_primitive_reserves_exit_then_reaps_exact_child_once() {
    let original = original_wait_primitive_child();
    let pid = original.id() as libc::pid_t;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match wait_reserved_child(pid).unwrap() {
            ChildWait::Running | ChildWait::Pending => {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            ChildWait::Exited { code, status } => {
                assert_eq!(code, libc::CLD_EXITED);
                assert_eq!(status, 23);
                break;
            }
        }
    }
    assert_eq!(
        wait_reserved_child(pid).unwrap(),
        ChildWait::Exited {
            code: libc::CLD_EXITED,
            status: 23
        }
    );
    let status = reap_reserved_child(pid).unwrap().unwrap();
    assert!(libc::WIFEXITED(status));
    assert_eq!(libc::WEXITSTATUS(status), 23);
    assert_eq!(
        reap_reserved_child(pid).unwrap_err().raw_os_error(),
        Some(libc::ECHILD)
    );
    drop(original);
}
#[cfg(feature = "desktop-test-support")]
#[test]
fn linux_original_wait_primitive_external_reap_returns_early_echild() {
    let mut original = original_wait_primitive_child();
    let pid = original.id() as libc::pid_t;
    assert_eq!(original.wait().unwrap().code(), Some(23));
    assert_eq!(
        wait_reserved_child(pid).unwrap_err().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn linux_original_reserved_exit_interrupt_yields_without_regression_model() {
    let invocation = OriginalInvocation { process: 17 };
    let mut child = OriginalGioChild::retain_callback(&invocation, 321);
    child
        .poll_with(|_| {
            Ok(ChildWait::Exited {
                code: libc::CLD_EXITED,
                status: 0,
            })
        })
        .unwrap();
    assert_eq!(
        child.poll_with(|_| Ok(ChildWait::Pending)).unwrap(),
        NativePrimaryState::Exited
    );
    assert!(matches!(child.state, ChildReservation::Exited { .. }));
}
