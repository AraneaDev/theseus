use super::*;
use crate::{model::*, snapshot::*};

fn fixture() -> (SuiteManifest, Vec<SourceReport>) {
    let suite = SuiteManifest {
        schema_version: 1,
        name: "budget".into(),
        baseline: "a".into(),
        profiles: vec![ProfileName::User],
        scenarios: vec![ScenarioKind::Clean],
        variants: ["a", "b"]
            .map(|id| Variant {
                id: id.into(),
                recipe: "fixture.toml".into(),
            })
            .into(),
        scopes: (0..8)
            .map(|i| Scope {
                id: format!("scope{i}"),
                profile: ProfileName::User,
                path: "/app".into(),
            })
            .collect(),
        stages: vec!["configured".into()],
    };
    let sources = ["a", "b"]
        .map(|id| {
            let mut report = RunReport::new(id);
            report.recipe_digest = "a".repeat(64);
            report.selected_profiles = suite.profiles.clone();
            report.selected_scenarios = suite.scenarios.clone();
            report.finished_at = Some("done".into());
            let mut scenario = ScenarioReport::new("clean", ProfileName::User, ScenarioKind::Clean);
            scenario.teardown = TeardownStatus::Complete;
            let mut stage = StageReport::new("configured");
            stage.outcome = StageOutcome::Passed;
            stage.snapshots = vec![Snapshot::missing("/app")];
            scenario.stages.push(stage);
            report.scenarios.push(scenario);
            report.recompute_outcome();
            SourceReport {
                variant_id: id.into(),
                report: Some(report),
                error: None,
            }
        })
        .into();
    (suite, sources)
}
fn snapshot(source: &mut SourceReport) -> &mut Snapshot {
    &mut source.report.as_mut().unwrap().scenarios[0].stages[0].snapshots[0]
}
fn assert_bounded_unknown(result: &ComparisonReport, limit: usize) {
    assert_eq!(result.outcome, RunOutcome::Incomplete);
    assert!(serde_json::to_vec_pretty(result).unwrap().len() <= limit);
    let first = result
        .cells
        .iter()
        .position(|c| c.comparison.status == ComparisonStatus::Unknown)
        .unwrap();
    assert!(
        result.cells[first..]
            .iter()
            .all(|c| c.comparison.status == ComparisonStatus::Unknown)
    );
    assert!(
        result.cells[first]
            .comparison
            .diagnostics
            .iter()
            .any(|s| s.contains("byte limit"))
    );
}
#[test]
fn derived_byte_limit_stops_metadata_and_path_amplification_with_prefix() {
    let (suite, mut sources) = fixture();
    for (index, source) in sources.iter_mut().enumerate() {
        let snap = snapshot(source);
        snap.completeness = ObservationStatus::Complete;
        snap.entries.insert(
            "/app".into(),
            FileEntry {
                kind: FileKind::Directory,
                mode: 0,
                uid: 0,
                gid: 0,
                size: 0,
                sha256: None,
                link_target: None,
            },
        );
        snap.entries.insert(
            format!("/app/{}", "p".repeat(256)),
            FileEntry {
                kind: FileKind::Symlink,
                mode: 0,
                uid: 0,
                gid: 0,
                size: 0,
                sha256: None,
                link_target: Some(format!("{index}{}", "\u{0001}".repeat(512))),
            },
        );
    }
    let result = derive_with_limit(&suite, &sources, 48 * 1024);
    assert_bounded_unknown(&result, 48 * 1024);
    assert!(!result.cells[0].comparison.differences.is_empty());
    assert!(
        result
            .cells
            .iter()
            .map(|c| c.comparison.differences.len())
            .sum::<usize>()
            < 8
    );
}
#[test]
fn derived_byte_limit_bounds_diagnostics_before_formatting() {
    let (suite, mut sources) = fixture();
    *snapshot(&mut sources[0]) = Snapshot::incomplete("/app", "\u{0001}".repeat(8192));
    let result = derive_with_limit(&suite, &sources, 32 * 1024);
    assert_bounded_unknown(&result, 32 * 1024);
    assert!(
        result
            .cells
            .iter()
            .all(|c| c.comparison.diagnostics.iter().all(|d| d.len() < 256))
    );
}
#[test]
fn derived_byte_limit_omits_summary_payload_without_losing_recovery_identity() {
    for mode in 0..4 {
        let (suite, mut sources) = fixture();
        let source = &mut sources[0];
        match mode {
            0 => source.report.as_mut().unwrap().image = "\u{0001}".repeat(8192),
            1 => source
                .report
                .as_mut()
                .unwrap()
                .artifacts
                .push(ArtifactDigest {
                    name: "artifact".into(),
                    sha256: "a".repeat(64),
                    files: vec![ArtifactFileDigest {
                        path: "p".repeat(8192),
                        sha256: "b".repeat(64),
                        size: 0,
                    }],
                }),
            2 => source.error = Some("e".repeat(8192)),
            _ => source.report.as_mut().unwrap().run_id = "r".repeat(8192),
        }
        let result = derive_with_limit(&suite, &sources, 32 * 1024);
        assert_bounded_unknown(&result, 32 * 1024);
        let summary = &result.sources[0];
        assert!(summary.error.as_ref().unwrap().contains("omitted"));
        assert!(summary.artifacts.is_empty());
        assert_eq!(
            summary.outcome,
            if mode == 2 {
                RunOutcome::Incomplete
            } else {
                RunOutcome::Passed
            }
        );
        assert_eq!(
            summary.recipe_digest.as_deref(),
            Some("a".repeat(64).as_str())
        );
        if mode != 3 {
            assert_eq!(summary.run_id.as_deref(), Some("a"));
        } else {
            assert!(summary.run_id.is_none());
        }
    }
}

#[test]
fn offline_and_live_share_budget_and_omission_keeps_contained_cleanup_refs() {
    let (suite, mut sources) = fixture();
    snapshot(&mut sources[0]).completeness = ObservationStatus::Incomplete {
        reason: "\u{0001}".repeat(8192),
    };
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("suite");
    std::fs::create_dir(&root).unwrap();
    let mut refs = vec![];
    for source in &sources {
        let report = source.report.as_ref().unwrap();
        let path = format!("sources/{}/{}", source.variant_id, report.run_id);
        let saved = crate::report::create(&root.join(&path)).unwrap();
        saved.save(report).unwrap();
        saved
            .save_journal(&RunJournal {
                schema_version: 1,
                run_id: report.run_id.clone(),
                resources: vec![],
                cleanup_errors: vec![],
            })
            .unwrap();
        refs.push(SourceReference {
            variant_id: source.variant_id.clone(),
            path,
            report_digest: Some(store::digest(&std::fs::read(saved.report_path()).unwrap())),
            error: None,
        });
    }
    let artifact = ComparisonArtifact {
        schema_version: 1,
        suite_id: "suite".into(),
        manifest_digest: store::manifest_digest(&suite).unwrap(),
        manifest: suite.clone(),
        recipe_digests: suite
            .variants
            .iter()
            .map(|v| (v.id.clone(), "a".repeat(64)))
            .collect(),
        sources: refs,
        report: derive_with_limit(&suite, &sources, 32 * 1024),
        cancelled: false,
        finished: true,
    };
    let saved = crate::report::open_existing(&root).unwrap();
    store::save(&saved, &artifact).unwrap();
    let live = store::recompute_with_limit(&artifact, &sources, 32 * 1024);
    let offline = store::load_document_with_limit(&root, 32 * 1024)
        .unwrap()
        .report;
    assert_bounded_unknown(&offline, 32 * 1024);
    assert_eq!(
        serde_json::to_value(&offline).unwrap(),
        serde_json::to_value(&live).unwrap()
    );
    assert_eq!(
        offline.sources[0].report_path.as_deref(),
        Some("sources/a/a/report.json")
    );
    super::cleanup(&root).unwrap(); // Empty owned journals require no Docker calls.
    for source in offline.sources {
        assert!(root.join(source.report_path.unwrap()).is_file());
    }
}
