use theseus_host::process::{ChildEnvironment, NativeCommand};
use theseus_native_helper::{CatalogEntry, Invocation};
use theseus_native_helper_store::StoreBinding;
use theseus_native_protocol::v2::*;
fn expected() -> StoreBinding {
    StoreBinding {
        binding: BindingId::from_bytes([1; 16]),
        manifest_digest: Digest::from_bytes([2; 32]),
        workspace_marker: WorkspaceMarker::from_bytes([3; 16]),
        os: if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Macos
        } else {
            Os::Linux
        },
    }
}
#[test]
fn explicit_existing_distinct_roots_and_fixed_environment() {
    let t = tempfile::tempdir().unwrap();
    let l = t.path().join("l");
    let w = t.path().join("w");
    std::fs::create_dir(&l).unwrap();
    std::fs::create_dir(&w).unwrap();
    let mut c = NativeCommand::new(if cfg!(windows) {
        "C:\\Windows\\System32\\whoami.exe"
    } else {
        "/usr/bin/true"
    });
    assert!(
        Invocation::new(
            l.clone(),
            w.clone(),
            expected(),
            vec![CatalogEntry {
                id: ProgramId::new("probe").unwrap(),
                command: c.clone()
            }]
        )
        .is_err()
    );
    c.environment = ChildEnvironment::Replace(vec![]);
    assert!(
        Invocation::new(
            l.clone(),
            w.clone(),
            expected(),
            vec![CatalogEntry {
                id: ProgramId::new("probe").unwrap(),
                command: c.clone()
            }]
        )
        .is_ok()
    );
    assert!(
        Invocation::new(
            l.clone(),
            l,
            expected(),
            vec![CatalogEntry {
                id: ProgramId::new("probe").unwrap(),
                command: c.clone()
            }]
        )
        .is_err()
    );
    assert!(
        Invocation::new(
            t.path().join("missing"),
            w,
            expected(),
            vec![CatalogEntry {
                id: ProgramId::new("probe").unwrap(),
                command: c
            }]
        )
        .is_err()
    );
}
#[test]
fn authority_rejects_noncanonical_catalog_and_native_configuration() {
    let t = tempfile::tempdir().unwrap();
    let l = t.path().join("l");
    let w = t.path().join("w");
    std::fs::create_dir(&l).unwrap();
    std::fs::create_dir(&w).unwrap();
    let fixed = || {
        let mut c = NativeCommand::new(if cfg!(windows) {
            "C:\\Windows\\System32\\whoami.exe"
        } else {
            "/usr/bin/true"
        });
        c.environment = ChildEnvironment::Replace(vec![]);
        c
    };
    let check = |catalog| Invocation::new(l.clone(), w.clone(), expected(), catalog).is_err();
    let entry = |id: &str, command| CatalogEntry {
        id: ProgramId::new(id).unwrap(),
        command,
    };
    assert!(check(vec![]));
    assert!(check(vec![entry("b", fixed()), entry("a", fixed())]));
    assert!(check(vec![entry("a", fixed()), entry("a", fixed())]));
    for c in [
        NativeCommand {
            program: "relative".into(),
            ..fixed()
        },
        NativeCommand {
            program: "/tmp/probe.cmd".into(),
            ..fixed()
        },
        NativeCommand {
            cwd: Some(w.clone()),
            ..fixed()
        },
        NativeCommand {
            args: vec!["x".repeat(513).into()],
            ..fixed()
        },
        NativeCommand {
            args: vec!["x".into(); 17],
            ..fixed()
        },
        NativeCommand {
            environment: ChildEnvironment::Replace(vec![("KEY".into(), "x".repeat(1025).into())]),
            ..fixed()
        },
    ] {
        assert!(check(vec![entry("probe", c)]));
    }
    let mut wrong = expected();
    wrong.os = if cfg!(windows) {
        Os::Linux
    } else {
        Os::Windows
    };
    assert!(Invocation::new(l, w, wrong, vec![entry("probe", fixed())]).is_err());
}
#[cfg(unix)]
#[test]
fn rejects_root_leaf_symlink_before_any_worker_authority() {
    let t = tempfile::tempdir().unwrap();
    let l = t.path().join("l");
    let w = t.path().join("w");
    std::fs::create_dir(&l).unwrap();
    std::fs::create_dir(&w).unwrap();
    let alias = t.path().join("alias");
    std::os::unix::fs::symlink(&l, &alias).unwrap();
    let mut c = NativeCommand::new("/usr/bin/true");
    c.environment = ChildEnvironment::Replace(vec![]);
    assert!(
        Invocation::new(
            alias,
            w,
            expected(),
            vec![CatalogEntry {
                id: ProgramId::new("probe").unwrap(),
                command: c
            }]
        )
        .is_err()
    );
}
#[cfg(unix)]
#[test]
fn ancestor_alias_is_permitted_and_real_open_error_is_retained() {
    use std::error::Error;
    let tmp = tempfile::tempdir().unwrap();
    let actual = tmp.path().join("actual");
    std::fs::create_dir(&actual).unwrap();
    std::fs::create_dir(actual.join("l")).unwrap();
    std::fs::create_dir(actual.join("w")).unwrap();
    let alias = tmp.path().join("alias");
    std::os::unix::fs::symlink(&actual, &alias).unwrap();
    let entry = || {
        let mut command = NativeCommand::new("/usr/bin/true");
        command.environment = ChildEnvironment::Replace(vec![]);
        CatalogEntry {
            id: ProgramId::new("probe").unwrap(),
            command,
        }
    };
    assert!(Invocation::new(alias.join("l"), alias.join("w"), expected(), vec![entry()]).is_ok());
    let error = match Invocation::new(
        alias.join("missing"),
        alias.join("w"),
        expected(),
        vec![entry()],
    ) {
        Err(e) => e,
        Ok(_) => panic!("missing directory accepted"),
    };
    assert!(error.source().is_some());
    assert_eq!(
        error
            .source()
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .unwrap()
            .kind(),
        std::io::ErrorKind::NotFound
    );
}
