use std::{collections::BTreeMap, slice::from_ref};
use theseus::{
    comparison::{ComparisonStatus, compare_scope},
    model::FileKind,
    snapshot::{FileEntry, ObservationStatus, Snapshot},
};
fn entry(kind: FileKind) -> FileEntry {
    FileEntry {
        kind,
        mode: 0o755,
        uid: 1,
        gid: 2,
        size: 0,
        sha256: None,
        link_target: None,
    }
}
fn tree(root: &str) -> Snapshot {
    Snapshot {
        root: root.into(),
        completeness: ObservationStatus::Complete,
        entries: BTreeMap::from([
            (root.into(), entry(FileKind::Directory)),
            (format!("{root}/child"), entry(FileKind::File)),
        ]),
    }
}
#[test]
fn complete_subtrees_compare_metadata_and_child_membership_with_relative_keys() {
    let before = tree("/app");
    assert_eq!(
        compare_scope(from_ref(&before), from_ref(&before), "/app").status,
        ComparisonStatus::Equivalent
    );
    for field in 0..7 {
        let mut after = before.clone();
        let e = after.entries.get_mut("/app/child").unwrap();
        match field {
            0 => e.mode = 0o600,
            1 => e.uid = 4,
            2 => e.gid = 5,
            3 => e.size = 6,
            4 => e.sha256 = Some("digest".into()),
            5 => e.link_target = Some("elsewhere".into()),
            _ => e.kind = FileKind::Symlink,
        }
        let result = compare_scope(from_ref(&before), &[after], "/app");
        assert_eq!(result.status, ComparisonStatus::Different);
        assert_eq!(result.differences[0].path, "child");
    }
    let mut after = before.clone();
    after.entries.remove("/app/child");
    assert_eq!(
        compare_scope(&[before], &[after], "/app").status,
        ComparisonStatus::Different
    );
}
#[test]
fn verified_absence_is_equivalent_but_presence_differs() {
    let missing = Snapshot::missing("/app");
    assert_eq!(
        compare_scope(from_ref(&missing), from_ref(&missing), "/app").status,
        ComparisonStatus::Equivalent
    );
    assert_eq!(
        compare_scope(&[missing], &[tree("/app")], "/app").status,
        ComparisonStatus::Different
    );
    let parent = tree("/app");
    assert_eq!(
        compare_scope(from_ref(&parent), from_ref(&parent), "/app/absent").status,
        ComparisonStatus::Equivalent
    );
}
#[test]
fn missing_incomplete_empty_overlapping_and_no_follow_evidence_is_unknown() {
    let valid = tree("/app");
    let mut empty = valid.clone();
    empty.entries.clear();
    let mut symlink = valid.clone();
    symlink.entries.get_mut("/app").unwrap().kind = FileKind::Symlink;
    for (snapshots, path) in [
        (vec![], "/app"),
        (vec![Snapshot::incomplete("/app", "limit")], "/app"),
        (vec![empty], "/app"),
        (
            vec![valid.clone(), Snapshot::missing("/app/nested")],
            "/app",
        ),
        (vec![valid.clone(), valid.clone()], "/app"),
        (vec![symlink], "/app/child"),
    ] {
        let result = compare_scope(&snapshots, from_ref(&valid), path);
        assert_eq!(result.status, ComparisonStatus::Unknown);
        assert!(!result.diagnostics.is_empty());
    }
}
#[test]
fn difference_overflow_is_unknown_and_retains_bounded_prefix() {
    let before = tree("/app");
    let mut after = before.clone();
    for n in 0..10_001 {
        after
            .entries
            .insert(format!("/app/new{n:05}"), entry(FileKind::File));
    }
    let result = compare_scope(&[before], &[after], "/app");
    assert_eq!(result.status, ComparisonStatus::Unknown);
    assert_eq!(result.differences.len(), 10_000);
}

use theseus::{
    comparison::{Scope, SourceReport, SuiteManifest, Variant, derive},
    model::{
        ProfileName, RunOutcome, RunReport, ScenarioKind, ScenarioReport, StageOutcome,
        StageReport, TeardownStatus,
    },
};
fn suite() -> SuiteManifest {
    SuiteManifest {
        schema_version: 1,
        name: "methods".into(),
        baseline: "copy".into(),
        profiles: vec![ProfileName::User],
        scenarios: vec![ScenarioKind::Clean],
        variants: vec![
            Variant {
                id: "copy".into(),
                recipe: "a.toml".into(),
            },
            Variant {
                id: "archive".into(),
                recipe: "b.toml".into(),
            },
        ],
        scopes: vec![Scope {
            id: "app".into(),
            profile: ProfileName::User,
            path: "/app".into(),
        }],
        stages: vec!["configured".into()],
    }
}
fn source(id: &str, snapshot: Snapshot) -> SourceReport {
    let mut report = RunReport::new(id);
    report.selected_profiles = vec![ProfileName::User];
    report.selected_scenarios = vec![ScenarioKind::Clean];
    report.finished_at = Some("done".into());
    let mut scenario = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
    scenario.teardown = TeardownStatus::Complete;
    let mut stage = StageReport::new("configured");
    stage.outcome = StageOutcome::Passed;
    stage.snapshots = vec![snapshot];
    scenario.stages.push(stage);
    report.scenarios.push(scenario);
    report.recompute_outcome();
    SourceReport {
        variant_id: id.into(),
        report: Some(report),
        error: None,
    }
}
#[test]
fn derive_compares_matrix_and_aggregates_source_failures_and_cell_differences() {
    let s = suite();
    let a = source("copy", tree("/app"));
    let b = source("archive", tree("/app"));
    let result = derive(&s, &[a.clone(), b.clone()]);
    assert_eq!(result.cells.len(), 1);
    assert_eq!(result.outcome, RunOutcome::Passed);
    let mut changed = b.clone();
    changed.report.as_mut().unwrap().scenarios[0].stages[0].snapshots[0]
        .entries
        .get_mut("/app/child")
        .unwrap()
        .uid = 999;
    assert_eq!(
        derive(&s, &[a.clone(), changed]).outcome,
        RunOutcome::Failed
    );
    let mut failed = b.clone();
    let r = failed.report.as_mut().unwrap();
    r.outcome = RunOutcome::Passed;
    r.scenarios[0].stages[0].outcome = StageOutcome::Failed;
    assert_eq!(derive(&s, &[a.clone(), failed]).outcome, RunOutcome::Failed);
    let mut cancelled = b.clone();
    cancelled.report.as_mut().unwrap().cancelled = true;
    assert_eq!(derive(&s, &[a, cancelled]).outcome, RunOutcome::Cancelled);
}
#[test]
fn derive_missing_and_duplicate_sources_scenarios_stages_and_selections_are_unknown() {
    let a = source("copy", tree("/app"));
    let b = source("archive", tree("/app"));
    let mut variants = vec![vec![a.clone()], vec![a.clone(), b.clone(), b.clone()]];
    for case in 0..7 {
        let mut altered = b.clone();
        let r = altered.report.as_mut().unwrap();
        match case {
            0 => r.scenarios.clear(),
            1 => r.scenarios.push(r.scenarios[0].clone()),
            2 => r.scenarios[0].stages.clear(),
            3 => {
                let duplicate = r.scenarios[0].stages[0].clone();
                r.scenarios[0].stages.push(duplicate)
            }
            4 => r.selected_profiles.clear(),
            5 => altered.report = None,
            _ => altered.error = Some("source load error".into()),
        }
        variants.push(vec![a.clone(), altered]);
    }
    for sources in variants {
        let result = derive(&suite(), &sources);
        assert_eq!(result.outcome, RunOutcome::Incomplete);
        assert_eq!(result.cells[0].comparison.status, ComparisonStatus::Unknown);
        assert!(!result.cells[0].comparison.diagnostics.is_empty());
    }
    let mut typo = suite();
    typo.stages = vec!["typo-stage".into()];
    assert_eq!(
        derive(&typo, &[a, b]).cells[0].comparison.status,
        ComparisonStatus::Unknown
    );
}
#[test]
fn derive_does_not_trust_cached_passed_or_unfinished_sources() {
    let a = source("copy", tree("/app"));
    let mut b = source("archive", tree("/app"));
    b.report.as_mut().unwrap().finished_at = None;
    assert_eq!(
        derive(&suite(), &[a.clone(), b.clone()]).outcome,
        RunOutcome::Incomplete
    );
    b.report.as_mut().unwrap().finished_at = Some("done".into());
    b.report
        .as_mut()
        .unwrap()
        .errors
        .push("infrastructure failure".into());
    assert_eq!(derive(&suite(), &[a, b]).outcome, RunOutcome::Incomplete);
}
#[test]
fn derive_bounds_matrix_before_allocation_and_stored_differences_across_suite() {
    let mut s = suite();
    s.stages = (0..33).map(|n| format!("s{n}")).collect();
    let result = derive(&s, &[]);
    assert!(result.cells.is_empty());
    assert_eq!(result.outcome, RunOutcome::Incomplete);
    let mut s = suite();
    let mut sources = vec![source("copy", tree("/app"))];
    let mut changed = tree("/app");
    for n in 0..10_000 {
        changed
            .entries
            .insert(format!("/app/new{n:05}"), entry(FileKind::File));
    }
    for n in 1..13 {
        let id = format!("v{n}");
        s.variants.push(Variant {
            id: id.clone(),
            recipe: "other.toml".into(),
        });
        sources.push(source(&id, changed.clone()));
    }
    s.variants.remove(1);
    let result = derive(&s, &sources);
    assert_eq!(result.cells.len(), 12);
    assert_eq!(
        result
            .cells
            .iter()
            .map(|c| c.comparison.differences.len())
            .sum::<usize>(),
        100_000
    );
    assert_eq!(
        result.cells[10].comparison.status,
        ComparisonStatus::Unknown
    );
    assert_eq!(
        result.cells[11].comparison.status,
        ComparisonStatus::Unknown
    );
    assert_eq!(result.outcome, RunOutcome::Incomplete);
}

#[test]
fn malformed_missing_roots_cannot_prove_absence() {
    let result = compare_scope(&[Snapshot::missing("")], &[Snapshot::missing("")], "/app");
    assert_eq!(result.status, ComparisonStatus::Unknown);
    let mut contradictory = Snapshot::missing("/app");
    contradictory
        .entries
        .insert("/app".into(), entry(FileKind::Directory));
    assert_eq!(
        compare_scope(&[contradictory], &[Snapshot::missing("/app")], "/app").status,
        ComparisonStatus::Unknown
    );
}
