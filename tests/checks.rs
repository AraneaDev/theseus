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

// These fixtures catch implementations that compare only the directory entry.
fn stability_snapshot(contents: &str) -> Snapshot {
    use sha2::{Digest, Sha256};
    use theseus::snapshot::{FileEntry, ObservationStatus};
    let directory = FileEntry {
        kind: FileKind::Directory,
        mode: 0o755,
        uid: 10001,
        gid: 10001,
        size: 0,
        sha256: None,
        link_target: None,
    };
    Snapshot {
        root: "/app".into(),
        completeness: ObservationStatus::Complete,
        entries: [
            ("/app".into(), directory.clone()),
            ("/app/config".into(), directory),
            (
                "/app/config/settings".into(),
                FileEntry {
                    kind: FileKind::File,
                    mode: 0o644,
                    uid: 10001,
                    gid: 10001,
                    size: contents.len() as u64,
                    sha256: Some(format!("{:x}", Sha256::digest(contents))),
                    link_target: None,
                },
            ),
        ]
        .into(),
    }
}

#[test]
fn a_changed_descendant_is_not_idempotent() {
    use theseus::checks::compare_unchanged;
    assert_eq!(
        compare_unchanged(
            "/app/config",
            &[stability_snapshot("old")],
            &[stability_snapshot("new")],
        )
        .outcome,
        CheckOutcome::Failed
    );
}

#[test]
fn identical_overwrites_and_existing_empty_directories_are_stable() {
    use theseus::checks::compare_unchanged;
    for path in ["/app/config", "/app/config/settings"] {
        assert_eq!(
            compare_unchanged(
                path,
                &[stability_snapshot("same")],
                &[stability_snapshot("same")]
            )
            .outcome,
            CheckOutcome::Passed,
        );
    }
    let mut empty = stability_snapshot("old");
    empty.entries.remove("/app/config/settings");
    assert_eq!(
        compare_unchanged("/app/config", &[empty.clone()], &[empty]).outcome,
        CheckOutcome::Passed,
    );
}

#[test]
fn stable_scopes_detect_metadata_types_and_child_set_changes() {
    use theseus::checks::compare_unchanged;
    let before = stability_snapshot("old");
    // Each mutation is independent: losing any compared metadata field or either
    // side of the descendant union would allow one of these cases to pass.
    for mutation in [
        "mode",
        "uid",
        "gid",
        "size",
        "kind",
        "add",
        "remove",
        "directory_mode",
    ] {
        let mut after = before.clone();
        match mutation {
            "add" => {
                let entry = after.entries["/app/config/settings"].clone();
                after.entries.insert("/app/config/extra".into(), entry);
            }
            "remove" => {
                after.entries.remove("/app/config/settings");
            }
            "directory_mode" => {
                after.entries.get_mut("/app/config").unwrap().mode = 0o700;
            }
            field => {
                let entry = after.entries.get_mut("/app/config/settings").unwrap();
                match field {
                    "mode" => entry.mode = 0o600,
                    "uid" => entry.uid = 0,
                    "gid" => entry.gid = 0,
                    "size" => entry.size = 4,
                    "kind" => entry.kind = FileKind::Directory,
                    _ => unreachable!(),
                }
            }
        }
        assert_eq!(
            compare_unchanged("/app/config", std::slice::from_ref(&before), &[after]).outcome,
            CheckOutcome::Failed,
            "{mutation}",
        );
    }
}

#[test]
fn sibling_prefix_changes_stay_outside_the_stable_scope() {
    use theseus::checks::compare_unchanged;
    let before = stability_snapshot("old");
    let mut after = before.clone();
    after.entries.insert(
        "/app/config-old".into(),
        after.entries["/app/config"].clone(),
    );
    after.entries.insert(
        "/app/config-old/extra".into(),
        after.entries["/app/config/settings"].clone(),
    );
    assert_eq!(
        compare_unchanged("/app/config", &[before], &[after]).outcome,
        CheckOutcome::Passed,
    );
}

#[test]
fn absent_baselines_and_deleted_scopes_cannot_establish_stability() {
    use theseus::checks::compare_unchanged;
    let present = stability_snapshot("old");
    let mut absent = present.clone();
    absent.entries.retain(|path, _| path == "/app");
    for (before, after, missing_stage) in [
        (&absent, &absent, "before"),
        (&absent, &present, "before"),
        (&present, &absent, "after"),
    ] {
        let result = compare_unchanged(
            "/app/config",
            std::slice::from_ref(before),
            std::slice::from_ref(after),
        );
        assert_eq!(result.outcome, CheckOutcome::Failed);
        assert!(result.message.contains(missing_stage), "{}", result.message);
        assert!(result.message.contains("absent"), "{}", result.message);
    }
    assert_eq!(
        compare_unchanged(
            "/app/config",
            &[Snapshot::missing("/app")],
            &[Snapshot::missing("/app")]
        )
        .outcome,
        CheckOutcome::Failed,
    );
}

#[test]
fn stability_requires_complete_unambiguous_observations_at_both_stages() {
    use theseus::checks::compare_unchanged;
    let observed = stability_snapshot("old");
    for (unavailable, reason) in [
        (vec![], "no observed root"),
        (
            vec![Snapshot::incomplete("/app", "permission denied")],
            "permission denied",
        ),
        (vec![Snapshot::missing("/elsewhere")], "no observed root"),
        (vec![observed.clone(), observed.clone()], "ambiguous"),
        (
            vec![observed.clone(), Snapshot::missing("/app/config")],
            "ambiguous",
        ),
        // The selected scope also covers this descendant observation. A check
        // of only its own exact path would overlook that ambiguous evidence.
        (
            vec![
                observed.clone(),
                Snapshot::incomplete("/app/config/nested", "denied"),
            ],
            "ambiguous",
        ),
    ] {
        for (before, after) in [
            (unavailable.as_slice(), std::slice::from_ref(&observed)),
            (std::slice::from_ref(&observed), unavailable.as_slice()),
        ] {
            let result = compare_unchanged("/app/config", before, after);
            assert_eq!(result.outcome, CheckOutcome::Incomplete, "{reason}");
            assert!(result.message.contains(reason), "{}", result.message);
        }
    }
    // Uncertainty takes precedence even when the other stage proves absence.
    assert_eq!(
        compare_unchanged(
            "/app/config",
            &[Snapshot::missing("/app")],
            &[Snapshot::incomplete("/app", "denied")]
        )
        .outcome,
        CheckOutcome::Incomplete,
    );
}

#[test]
fn stability_cannot_traverse_current_or_reference_non_directory_ancestors() {
    use theseus::checks::compare_unchanged;
    let observed = stability_snapshot("old");
    for ancestor in ["/app", "/app/config"] {
        for kind in [FileKind::Symlink, FileKind::File] {
            let mut blocked = observed.clone();
            let entry = blocked.entries.get_mut(ancestor).unwrap();
            entry.kind = kind;
            entry.link_target = (kind == FileKind::Symlink).then(|| "/outside".into());
            blocked
                .entries
                .retain(|path, _| !path.starts_with(&format!("{ancestor}/")));
            for (before, after) in [(&blocked, &observed), (&observed, &blocked)] {
                let result = compare_unchanged(
                    "/app/config/settings",
                    std::slice::from_ref(before),
                    std::slice::from_ref(after),
                );
                assert_eq!(result.outcome, CheckOutcome::Incomplete);
                assert!(
                    result.message.contains("non-directory ancestor"),
                    "{}",
                    result.message
                );
            }
        }
    }
}

#[test]
fn a_symlinks_own_stability_compares_target_text_without_following_it() {
    use theseus::checks::compare_unchanged;
    let mut before = stability_snapshot("old");
    let link = before.entries.get_mut("/app/config/settings").unwrap();
    link.kind = FileKind::Symlink;
    link.size = 0;
    link.sha256 = None;
    link.link_target = Some("/outside/missing".into());
    let mut after = before.clone();
    // Complete observations of the target are irrelevant to link stability.
    let outside = Snapshot::incomplete("/outside", "unreadable target");
    assert_eq!(
        compare_unchanged(
            "/app/config/settings",
            &[before.clone()],
            &[after.clone(), outside]
        )
        .outcome,
        CheckOutcome::Passed,
    );
    after
        .entries
        .get_mut("/app/config/settings")
        .unwrap()
        .link_target = Some("/another/missing".into());
    assert_eq!(
        compare_unchanged("/app/config/settings", &[before], &[after]).outcome,
        CheckOutcome::Failed,
    );
}

#[test]
fn preserved_directories_still_compare_only_the_exact_configured_entry() {
    let configured = [stability_snapshot("old")];
    let current = [stability_snapshot("new")];
    let preserved = check(Expectation::Preserved {
        path: "/app/config".into(),
    });
    assert_eq!(
        evaluate_filesystem(&preserved, &current, Some(&configured)).outcome,
        CheckOutcome::Passed,
    );
}

#[test]
fn stability_summary_bounds_large_observation_errors() {
    use theseus::checks::compare_unchanged;
    let result = compare_unchanged(
        "/app/config",
        &[Snapshot::incomplete("/app", "denied α ".repeat(1000))],
        &[stability_snapshot("old")],
    );
    assert_eq!(result.outcome, CheckOutcome::Incomplete);
    assert!(
        result
            .message
            .starts_with("before: observation of /app is incomplete: denied")
    );
    assert!(
        result.message.len() <= 1024,
        "summary expanded to {} bytes",
        result.message.len()
    );
}
