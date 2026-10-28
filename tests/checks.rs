mod support;
use support::Tar;
use theseus::{
    checks::{evaluate_filesystem, review_findings},
    model::{CheckOutcome, Checkpoint, FileKind, FindingKind},
    recipe::{Check, Expectation},
    snapshot::{Limits, Snapshot, read_archive},
};
fn snapshot(tar: Tar) -> Snapshot {
    read_archive(&tar.finish()[..], "/usr/local/bin", Limits::default()).unwrap()
}
fn check(expectation: Expectation) -> Check {
    Check {
        id: "contract".into(),
        checkpoint: Checkpoint::Uninstalled,
        expectation,
    }
}
#[test]
fn dangling_links_are_present_and_absence_requires_their_removal() {
    let s = [snapshot(Tar::new().root().entry(
        "bin/dangling",
        b'2',
        b"",
        0o777,
        0,
        0,
        "/missing",
    ))];
    let absent = check(Expectation::Absent {
        path: "/usr/local/bin/dangling".into(),
    });
    assert_eq!(
        evaluate_filesystem(&absent, &s, None).outcome,
        CheckOutcome::Failed
    );
    let present = check(Expectation::Present {
        path: "/usr/local/bin/dangling".into(),
        expected_kind: Some(FileKind::Symlink),
    });
    assert_eq!(
        evaluate_filesystem(&present, &s, None).outcome,
        CheckOutcome::Passed
    );
    let wrong_kind = check(Expectation::Present {
        path: "/usr/local/bin/dangling".into(),
        expected_kind: Some(FileKind::File),
    });
    assert_eq!(
        evaluate_filesystem(&wrong_kind, &s, None).outcome,
        CheckOutcome::Failed
    );
    assert_eq!(
        evaluate_filesystem(&absent, &[snapshot(Tar::new().root())], None).outcome,
        CheckOutcome::Passed
    );
}
#[test]
fn preservation_compares_configured_state_including_metadata() {
    let configured = [snapshot(
        Tar::new().root().file("bin/config", b"user setting"),
    )];
    let c = check(Expectation::Preserved {
        path: "/usr/local/bin/config".into(),
    });
    assert_eq!(
        evaluate_filesystem(&c, &configured, Some(&configured)).outcome,
        CheckOutcome::Passed
    );
    let modified = [snapshot(Tar::new().root().entry(
        "bin/config",
        b'0',
        b"user setting",
        0o600,
        10001,
        10001,
        "",
    ))];
    assert_eq!(
        evaluate_filesystem(&c, &modified, Some(&configured)).outcome,
        CheckOutcome::Failed
    );
    assert_eq!(
        evaluate_filesystem(&c, &configured, None).outcome,
        CheckOutcome::Incomplete
    );
    let absent = [snapshot(Tar::new().root())];
    assert_eq!(
        evaluate_filesystem(&c, &absent, Some(&absent)).outcome,
        CheckOutcome::Failed
    );
}
#[test]
fn unavailable_observation_cannot_prove_absence_but_missing_root_can() {
    let c = check(Expectation::Absent {
        path: "/usr/local/bin/tool".into(),
    });
    assert_eq!(
        evaluate_filesystem(
            &c,
            &[Snapshot::incomplete("/usr/local/bin", "permission denied")],
            None
        )
        .outcome,
        CheckOutcome::Incomplete
    );
    assert_eq!(
        evaluate_filesystem(&c, &[], None).outcome,
        CheckOutcome::Incomplete
    );
    assert_eq!(
        evaluate_filesystem(&c, &[Snapshot::missing("/usr/local/bin")], None).outcome,
        CheckOutcome::Passed
    );
}
#[test]
fn reviews_unknown_leftovers_and_baseline_deletion_but_honors_exact_retention() {
    let before = [snapshot(
        Tar::new()
            .root()
            .file("bin/baseline", b"base")
            .file("bin/untouched", b"base"),
    )];
    let after = [snapshot(
        Tar::new()
            .root()
            .file("bin/untouched", b"base")
            .file("bin/retained", b"data")
            .file("bin/unknown", b"leftover"),
    )];
    let retained = check(Expectation::Present {
        path: "/usr/local/bin/retained".into(),
        expected_kind: Some(FileKind::File),
    });
    let findings = review_findings(&before, &after, &[retained]);
    assert_eq!(
        findings
            .iter()
            .map(|f| (f.path.as_deref().unwrap(), f.kind))
            .collect::<Vec<_>>(),
        vec![
            ("/usr/local/bin/baseline", FindingKind::BaselineDeletion),
            ("/usr/local/bin/unknown", FindingKind::Leftover)
        ]
    );
    let allowed = check(Expectation::Absent {
        path: "/usr/local/bin/baseline".into(),
    });
    let findings = review_findings(&before, &after, &[allowed]);
    assert_eq!(findings.len(), 2);
    assert!(findings.iter().all(|f| f.kind == FindingKind::Leftover));
}
#[test]
fn command_and_executable_expectations_compare_literal_evidence() {
    use theseus::checks::{CommandObservation, evaluate_command, evaluate_executable};
    let c = check(Expectation::Command {
        script: "tool".into(),
        timeout_seconds: 120,
        env: Default::default(),
        pass_env: vec![],
        expected_exit: 3,
        stdout_contains: Some("ready.*".into()),
    });
    let observed = CommandObservation {
        exit_status: Some(3),
        stdout: "ready.*".into(),
        complete: true,
        error: None,
    };
    assert_eq!(
        evaluate_command(&c, &observed).outcome,
        CheckOutcome::Passed
    );
    assert_eq!(
        evaluate_command(
            &c,
            &CommandObservation {
                stdout: "ready!".into(),
                ..observed.clone()
            }
        )
        .outcome,
        CheckOutcome::Failed
    );
    assert_eq!(
        evaluate_command(
            &c,
            &CommandObservation {
                complete: false,
                ..observed.clone()
            }
        )
        .outcome,
        CheckOutcome::Incomplete
    );
    let c = check(Expectation::Executable {
        name: "tool".into(),
        resolved_path: "/usr/local/bin/tool".into(),
        args: vec!["--version".into()],
        timeout_seconds: 120,
        env: Default::default(),
        pass_env: vec![],
        expected_exit: 3,
        stdout_contains: Some("ready.*".into()),
    });
    assert_eq!(
        evaluate_executable(&c, Some("/usr/local/bin/tool"), &observed).outcome,
        CheckOutcome::Passed
    );
    assert_eq!(
        evaluate_executable(&c, Some("/bin/tool"), &observed).outcome,
        CheckOutcome::Failed
    );
}

#[test]
fn paths_below_symlink_or_file_ancestors_are_unobserved() {
    for snapshot in [
        snapshot(
            Tar::new()
                .root()
                .entry("bin/link", b'2', b"", 0o777, 0, 0, "/outside"),
        ),
        snapshot(Tar::new().root().file("bin/link", b"not a directory")),
    ] {
        let snapshots = [snapshot];
        for expectation in [
            Expectation::Absent {
                path: "/usr/local/bin/link/child".into(),
            },
            Expectation::Present {
                path: "/usr/local/bin/link/child".into(),
                expected_kind: None,
            },
            Expectation::Preserved {
                path: "/usr/local/bin/link/child".into(),
            },
        ] {
            assert_eq!(
                evaluate_filesystem(&check(expectation), &snapshots, Some(&snapshots)).outcome,
                CheckOutcome::Incomplete,
            );
        }
    }
}

#[test]
fn symlink_observed_root_does_not_establish_its_descendants() {
    let snapshots = [snapshot(
        Tar::new().entry("bin", b'2', b"", 0o777, 0, 0, "/outside"),
    )];
    for expectation in [
        Expectation::Absent {
            path: "/usr/local/bin/child".into(),
        },
        Expectation::Present {
            path: "/usr/local/bin/child".into(),
            expected_kind: None,
        },
        Expectation::Preserved {
            path: "/usr/local/bin/child".into(),
        },
    ] {
        assert_eq!(
            evaluate_filesystem(&check(expectation), &snapshots, Some(&snapshots)).outcome,
            CheckOutcome::Incomplete,
        );
    }
    let root = check(Expectation::Present {
        path: "/usr/local/bin".into(),
        expected_kind: Some(FileKind::Symlink),
    });
    assert_eq!(
        evaluate_filesystem(&root, &snapshots, None).outcome,
        CheckOutcome::Passed
    );
}

#[test]
fn preservation_requires_observed_configured_ancestors() {
    let current = [snapshot(
        Tar::new()
            .root()
            .entry("bin/link", b'5', b"", 0o755, 0, 0, "")
            .file("bin/link/child", b"new"),
    )];
    let configured = [snapshot(
        Tar::new()
            .root()
            .entry("bin/link", b'2', b"", 0o777, 0, 0, "/outside"),
    )];
    let c = check(Expectation::Preserved {
        path: "/usr/local/bin/link/child".into(),
    });
    assert_eq!(
        evaluate_filesystem(&c, &current, Some(&configured)).outcome,
        CheckOutcome::Incomplete
    );
}
