use std::collections::BTreeMap;
use theseus::{
    checks::{compare_changed, compare_mapped},
    model::{CheckOutcome, FileKind},
    snapshot::{FileEntry, ObservationStatus, Snapshot},
};

// Handwritten observations: the backup ancestor deliberately has different
// metadata. Expected outcomes never depend on diff or a production helper.
fn fixtures() -> (Vec<Snapshot>, Vec<Snapshot>) {
    let directory = FileEntry {
        kind: FileKind::Directory,
        mode: 493,
        uid: 10001,
        gid: 10001,
        size: 0,
        sha256: None,
        link_target: None,
    };
    let file = FileEntry {
        kind: FileKind::File,
        mode: 420,
        uid: 10001,
        gid: 10001,
        size: 3,
        sha256: Some("a".repeat(64)),
        link_target: None,
    };
    let known = vec![Snapshot {
        root: "/app".into(),
        completeness: ObservationStatus::Complete,
        entries: BTreeMap::from([
            ("/app".into(), directory.clone()),
            ("/app/data".into(), directory.clone()),
            ("/app/data/records".into(), file.clone()),
        ]),
    }];
    let mut scaffold = directory.clone();
    scaffold.mode = 448;
    let copy = vec![Snapshot {
        root: "/backup".into(),
        completeness: ObservationStatus::Complete,
        entries: BTreeMap::from([
            ("/backup".into(), scaffold),
            ("/backup/data".into(), directory),
            ("/backup/data/records".into(), file),
        ]),
    }];
    (known, copy)
}

#[test]
fn mapped_subtrees_ignore_only_ancestor_scaffolding_and_include_child_hashes() {
    let (known, copy) = fixtures();
    assert_eq!(
        compare_mapped("/app/data", "/backup/data", &known, &copy).outcome,
        CheckOutcome::Passed
    );
    let mut bad_copy = copy;
    bad_copy[0]
        .entries
        .get_mut("/backup/data/records")
        .unwrap()
        .sha256 = Some("b".repeat(64));
    assert_eq!(
        compare_mapped("/app/data", "/backup/data", &known, &bad_copy).outcome,
        CheckOutcome::Failed
    );
}

#[test]
fn changed_requires_a_real_difference_in_the_declared_subtree() {
    let (known, _) = fixtures();
    assert_eq!(
        compare_changed("/app/data", &known, &known).outcome,
        CheckOutcome::Failed
    );
    let mut changed = known.clone();
    changed[0]
        .entries
        .get_mut("/app/data/records")
        .unwrap()
        .sha256 = Some("b".repeat(64));
    assert_eq!(
        compare_changed("/app/data", &known, &changed).outcome,
        CheckOutcome::Passed
    );
}

#[test]
fn observed_creation_and_removal_establish_change_but_two_absences_do_not() {
    let (known, _) = fixtures();
    let mut absent = known.clone();
    absent[0].entries.retain(|path, _| path == "/app");
    for missing in [absent, vec![Snapshot::missing("/app")]] {
        assert_eq!(
            compare_changed("/app/data", &missing, &known).outcome,
            CheckOutcome::Passed
        );
        assert_eq!(
            compare_changed("/app/data", &known, &missing).outcome,
            CheckOutcome::Passed
        );
        assert_eq!(
            compare_changed("/app/data", &missing, &missing).outcome,
            CheckOutcome::Failed
        );
    }
}

#[test]
fn every_entry_field_and_both_child_sets_participate_in_comparisons() {
    let (known, copy) = fixtures();
    // Each independent mutation catches omission of one equality field or one
    // side of the child union, including the mapped root directory metadata.
    for field in [
        "mode",
        "uid",
        "gid",
        "size",
        "kind",
        "hash",
        "link",
        "add",
        "remove",
        "root_mode",
    ] {
        let mut changed = known.clone();
        let mut bad_copy = copy.clone();
        for (snapshot, root) in [
            (&mut changed[0], "/app/data"),
            (&mut bad_copy[0], "/backup/data"),
        ] {
            let path = format!("{root}/records");
            match field {
                "add" => {
                    snapshot
                        .entries
                        .insert(format!("{root}/extra"), snapshot.entries[&path].clone());
                }
                "remove" => {
                    snapshot.entries.remove(&path);
                }
                "root_mode" => snapshot.entries.get_mut(root).unwrap().mode = 448,
                field => {
                    let entry = snapshot.entries.get_mut(&path).unwrap();
                    match field {
                        "mode" => entry.mode = 384,
                        "uid" => entry.uid = 0,
                        "gid" => entry.gid = 0,
                        "size" => entry.size = 4,
                        "kind" => entry.kind = FileKind::Directory,
                        "hash" => entry.sha256 = Some("b".repeat(64)),
                        "link" => entry.link_target = Some("/elsewhere".into()),
                        _ => unreachable!(),
                    }
                }
            }
        }
        assert_eq!(
            compare_changed("/app/data", &known, &changed).outcome,
            CheckOutcome::Passed,
            "{field}"
        );
        assert_eq!(
            compare_mapped("/app/data", "/backup/data", &known, &bad_copy).outcome,
            CheckOutcome::Failed,
            "{field}"
        );
    }
}

#[test]
fn mapped_files_and_empty_directories_compare_their_own_entries() {
    let (mut known, mut copy) = fixtures();
    assert_eq!(
        compare_mapped("/app/data/records", "/backup/data/records", &known, &copy).outcome,
        CheckOutcome::Passed
    );
    known[0].entries.remove("/app/data/records");
    copy[0].entries.remove("/backup/data/records");
    assert_eq!(
        compare_mapped("/app/data", "/backup/data", &known, &copy).outcome,
        CheckOutcome::Passed
    );
}

#[test]
fn sibling_prefixes_do_not_enter_either_comparison_scope() {
    let (known, mut copy) = fixtures();
    let mut changed = known.clone();
    changed[0].entries.insert(
        "/app/data-old".into(),
        known[0].entries["/app/data/records"].clone(),
    );
    copy[0].entries.insert(
        "/backup/data-old/records".into(),
        known[0].entries["/app/data/records"].clone(),
    );
    assert_eq!(
        compare_changed("/app/data", &known, &changed).outcome,
        CheckOutcome::Failed
    );
    assert_eq!(
        compare_mapped("/app/data", "/backup/data", &known, &copy).outcome,
        CheckOutcome::Passed
    );
}

#[test]
fn mapped_comparison_requires_existing_source_and_copy() {
    let (known, copy) = fixtures();
    for (source, stored) in [
        ("/app/absent", "/backup/data"),
        ("/app/data", "/backup/absent"),
        ("/app/absent", "/backup/absent"),
    ] {
        assert_eq!(
            compare_mapped(source, stored, &known, &copy).outcome,
            CheckOutcome::Failed
        );
    }
    for (source, stored) in [
        (vec![Snapshot::missing("/app")], copy.clone()),
        (known, vec![Snapshot::missing("/backup")]),
        (
            vec![Snapshot::missing("/app")],
            vec![Snapshot::missing("/backup")],
        ),
    ] {
        assert_eq!(
            compare_mapped("/app/data", "/backup/data", &source, &stored).outcome,
            CheckOutcome::Failed
        );
    }
}

#[test]
fn uncertainty_on_either_side_wins_over_absence_or_apparent_change() {
    let (known, _) = fixtures();
    let missing = vec![Snapshot::missing("/app")];
    for uncertain in [
        vec![],
        vec![Snapshot::missing("/elsewhere")],
        vec![Snapshot::incomplete("/app", "denied")],
        vec![known[0].clone(), known[0].clone()],
        vec![known[0].clone(), Snapshot::missing("/app/data")],
        vec![
            known[0].clone(),
            Snapshot::incomplete("/app/data/nested", "denied"),
        ],
    ] {
        for certain in [&known, &missing] {
            for (left, right) in [(&uncertain, certain), (certain, &uncertain)] {
                assert_eq!(
                    compare_changed("/app/data", left, right).outcome,
                    CheckOutcome::Incomplete
                );
                // Same absolute paths are a valid identity mapping and isolate
                // coverage precedence from the rebasing tested above.
                assert_eq!(
                    compare_mapped("/app/data", "/app/data", left, right).outcome,
                    CheckOutcome::Incomplete
                );
            }
        }
    }
}

#[test]
fn non_directory_ancestors_are_uncertain_at_either_stage() {
    let (known, _) = fixtures();
    for ancestor in ["/app", "/app/data"] {
        for kind in [FileKind::Symlink, FileKind::File] {
            let mut blocked = known.clone();
            blocked[0].entries.get_mut(ancestor).unwrap().kind = kind;
            blocked[0]
                .entries
                .retain(|path, _| !path.starts_with(&format!("{ancestor}/")));
            for (left, right) in [(&blocked, &known), (&known, &blocked)] {
                assert_eq!(
                    compare_changed("/app/data/records", left, right).outcome,
                    CheckOutcome::Incomplete
                );
                assert_eq!(
                    compare_mapped("/app/data/records", "/app/data/records", left, right).outcome,
                    CheckOutcome::Incomplete
                );
            }
        }
    }
}

#[test]
fn links_compare_target_text_without_observing_the_target() {
    let (mut known, mut copy) = fixtures();
    for (snapshots, path) in [
        (&mut known, "/app/data/records"),
        (&mut copy, "/backup/data/records"),
    ] {
        let entry = snapshots[0].entries.get_mut(path).unwrap();
        entry.kind = FileKind::Symlink;
        entry.mode = 511;
        entry.size = 0;
        entry.sha256 = None;
        entry.link_target = Some("/outside/secret".into());
        snapshots.push(Snapshot::incomplete("/outside", "target unreadable"));
    }
    assert_eq!(
        compare_mapped("/app/data/records", "/backup/data/records", &known, &copy).outcome,
        CheckOutcome::Passed
    );
    assert_eq!(
        compare_changed("/app/data/records", &known, &known).outcome,
        CheckOutcome::Failed
    );
    let mut changed = known.clone();
    changed[0]
        .entries
        .get_mut("/app/data/records")
        .unwrap()
        .link_target = Some("/elsewhere/secret".into());
    copy[0]
        .entries
        .get_mut("/backup/data/records")
        .unwrap()
        .link_target = Some("/elsewhere/secret".into());
    assert_eq!(
        compare_changed("/app/data/records", &known, &changed).outcome,
        CheckOutcome::Passed
    );
    assert_eq!(
        compare_mapped("/app/data/records", "/backup/data/records", &known, &copy).outcome,
        CheckOutcome::Failed
    );
}

#[test]
fn summaries_are_bounded_utf8_without_entry_fingerprints_or_link_text() {
    let (known, copy) = fixtures();
    let uncertain = [Snapshot::incomplete("/app", "denied α ".repeat(1000))];
    for result in [
        compare_changed("/app/data", &uncertain, &known),
        compare_mapped("/app/data", "/backup/data", &uncertain, &copy),
    ] {
        assert_eq!(result.outcome, CheckOutcome::Incomplete);
        assert!(result.message.len() <= 1024);
        assert!(result.message.starts_with("before:") || result.message.starts_with("source:"));
    }
    let mut changed = known.clone();
    changed[0]
        .entries
        .get_mut("/app/data/records")
        .unwrap()
        .sha256 = Some("sensitive-fingerprint".into());
    let mut bad_copy = copy;
    bad_copy[0]
        .entries
        .get_mut("/backup/data/records")
        .unwrap()
        .link_target = Some("sensitive-link-text".into());
    for result in [
        compare_changed("/app/data", &known, &changed),
        compare_mapped("/app/data", "/backup/data", &known, &bad_copy),
    ] {
        assert!(!result.message.contains("sensitive"));
    }
}
