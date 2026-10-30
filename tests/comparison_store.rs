#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
#[cfg(windows)]
fn symlink(
    source: impl AsRef<std::path::Path>,
    target: impl AsRef<std::path::Path>,
) -> std::io::Result<()> {
    if source.as_ref().is_dir() {
        std::os::windows::fs::symlink_dir(source, target)
    } else {
        std::os::windows::fs::symlink_file(source, target)
    }
}
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
use theseus::{
    comparison::{self, *},
    model::*,
    report,
    snapshot::Snapshot,
};

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("suite");
    fs::create_dir(&root).unwrap();
    let manifest = SuiteManifest {
        schema_version: 1,
        name: "methods".into(),
        baseline: "copy".into(),
        profiles: vec![ProfileName::User],
        scenarios: vec![ScenarioKind::Clean],
        variants: vec![
            Variant {
                id: "copy".into(),
                recipe: "deleted/a.toml".into(),
            },
            Variant {
                id: "archive".into(),
                recipe: "deleted/b.toml".into(),
            },
        ],
        scopes: vec![Scope {
            id: "app".into(),
            profile: ProfileName::User,
            path: "/app".into(),
        }],
        stages: vec!["configured".into()],
    };
    let mut sources = vec![];
    for id in ["copy", "archive"] {
        let path = format!("sources/{id}/run-{id}");
        let store = report::create(&root.join(&path)).unwrap();
        let mut r = RunReport::new(format!("run-{id}"));
        r.recipe_digest = "a".repeat(64);
        r.selected_profiles = manifest.profiles.clone();
        r.selected_scenarios = manifest.scenarios.clone();
        r.finished_at = Some("done".into());
        r.image_id = Some("sha256:fixture".into());
        let mut scenario =
            ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
        scenario.teardown = TeardownStatus::Complete;
        let mut stage = StageReport::new("configured");
        stage.outcome = StageOutcome::Passed;
        stage.snapshots = vec![Snapshot::missing("/app")];
        scenario.stages.push(stage);
        r.scenarios.push(scenario);
        store.save(&r).unwrap();
        store
            .save_journal(&RunJournal {
                schema_version: 1,
                run_id: r.run_id,
                resources: vec![],
                cleanup_errors: vec![],
            })
            .unwrap();
        sources.push(serde_json::json!({"variant_id": id, "path": path, "report_digest": digest(&fs::read(store.report_path()).unwrap()), "error": null}));
    }
    let artifact = serde_json::json!({"schema_version": 1, "suite_id": "suite", "manifest_digest": digest(&serde_json::to_vec(&manifest).unwrap()), "manifest": manifest,
        "recipe_digests": {"copy": "a".repeat(64), "archive": "a".repeat(64)}, "sources": sources,
        "report": {"cells": [], "outcome": "passed", "diagnostics": []}, "cancelled": false, "finished": true});
    fs::write(
        root.join("comparison.json"),
        serde_json::to_vec(&artifact).unwrap(),
    )
    .unwrap();
    (temp, root)
}
fn edit(root: &Path, f: impl FnOnce(&mut serde_json::Value)) {
    let path = root.join("comparison.json");
    let mut value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    f(&mut value);
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}
fn edit_source(root: &Path, f: impl FnOnce(&mut serde_json::Value), bind: bool) {
    let path = root.join("sources/archive/run-archive/report.json");
    let mut value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    f(&mut value);
    let bytes = serde_json::to_vec(&value).unwrap();
    fs::write(path, &bytes).unwrap();
    if bind {
        edit(root, |v| {
            v["sources"][1]["report_digest"] = digest(&bytes).into()
        });
    }
}
#[test]
fn offline_recomputes_forged_success_and_does_not_need_original_project() {
    let (_t, root) = fixture();
    assert_eq!(
        comparison::load_report(&root).unwrap().outcome,
        RunOutcome::Passed
    );
    let mut forged = serde_json::to_value(comparison::load_report(&root).unwrap()).unwrap();
    forged["sources"][0]["image_id"] = "forged".into();
    forged["sources"][0]["outcome"] = "cancelled".into();
    edit(&root, |v| v["report"] = forged);
    let recomputed = comparison::load_report(&root).unwrap();
    assert_eq!(
        recomputed.sources[0].image_id.as_deref(),
        Some("sha256:fixture")
    );
    assert_eq!(recomputed.sources[0].outcome, RunOutcome::Passed);
    edit_source(
        &root,
        |v| v["scenarios"][0]["stages"][0]["outcome"] = "failed".into(),
        true,
    );
    assert_eq!(
        comparison::load_report(&root).unwrap().outcome,
        RunOutcome::Failed
    );
    edit_source(
        &root,
        |v| v["scenarios"][0]["stages"] = serde_json::json!([]),
        true,
    );
    assert_eq!(
        comparison::load_report(&root).unwrap().outcome,
        RunOutcome::Incomplete
    );
}
#[test]
fn replaced_missing_and_unbound_sources_never_pass() {
    let (_t, root) = fixture();
    edit_source(&root, |v| v["image_id"] = "replaced".into(), false);
    assert!(comparison::load_report(&root).is_err());
    edit(&root, |v| {
        v["sources"][1]["report_digest"] = serde_json::Value::Null
    });
    assert_eq!(
        comparison::load_report(&root).unwrap().outcome,
        RunOutcome::Incomplete
    );
    fs::remove_file(root.join("sources/archive/run-archive/report.json")).unwrap();
    assert_eq!(
        comparison::load_report(&root).unwrap().outcome,
        RunOutcome::Incomplete
    );
}
#[test]
fn unfinished_missing_report_keeps_recorded_source_error() {
    let (_t, root) = fixture();
    edit(&root, |v| {
        v["sources"][1]["report_digest"] = serde_json::Value::Null;
        v["sources"][1]["error"] = "original save failure".into();
    });
    fs::remove_file(root.join("sources/archive/run-archive/report.json")).unwrap();
    let r = comparison::load_report(&root).unwrap();
    assert_eq!(r.outcome, RunOutcome::Incomplete);
    assert!(
        r.sources[1]
            .error
            .as_ref()
            .unwrap()
            .contains("original save failure")
    );
}
#[test]
fn source_recipe_run_and_matrix_identity_are_checked() {
    for (field, value) in [
        ("recipe_digest", serde_json::json!("b".repeat(64))),
        ("run_id", serde_json::json!("other")),
        ("selected_profiles", serde_json::json!(["system"])),
    ] {
        let (_t, root) = fixture();
        edit_source(&root, |v| v[field] = value, true);
        assert!(comparison::load_report(&root).is_err(), "{field}");
    }
}
#[test]
fn schema_manifest_bounds_and_duplicate_refs_are_rejected() {
    for mutation in 0..6 {
        let (_t, root) = fixture();
        edit(&root, |v| match mutation {
            0 => v["schema_version"] = 2.into(),
            1 => v["manifest"]["stages"] = serde_json::json!([]),
            2 => {
                let duplicate = v["sources"][0].clone();
                v["sources"].as_array_mut().unwrap().push(duplicate);
            }
            3 => v["manifest_digest"] = "b".repeat(64).into(),
            4 => v["suite_id"] = "../escape".into(),
            _ => v["sources"] = serde_json::Value::Array(vec![v["sources"][0].clone(); 17]),
        });
        assert!(
            comparison::load_report(&root).is_err(),
            "mutation {mutation}"
        );
    }
}
#[test]
fn unsafe_references_and_symlink_components_are_rejected_for_read_and_cleanup() {
    for path in [
        "../outside",
        "/tmp/run",
        "sources/copy/../outside",
        "sources/archive/run-copy",
        "sources/copy/.hidden",
        "sources/copy/run-copy/extra",
    ] {
        let (_t, root) = fixture();
        edit(&root, |v| v["sources"][0]["path"] = path.into());
        assert!(comparison::load_report(&root).is_err(), "{path}");
        assert!(comparison::cleanup(&root).is_err());
    }
    for relative in [
        "sources",
        "sources/copy",
        "sources/copy/run-copy",
        "sources/copy/run-copy/report.json",
        "comparison.json",
    ] {
        let (t, root) = fixture();
        let path = root.join(relative);
        let moved = t.path().join("outside");
        fs::rename(&path, &moved).unwrap();
        symlink(&moved, &path).unwrap();
        assert!(comparison::load_report(&root).is_err(), "{relative}");
        if !relative.ends_with("report.json") {
            assert!(comparison::cleanup(&root).is_err(), "{relative}");
        }
    }
    let (t, root) = fixture();
    symlink(&root, t.path().join("alias")).unwrap();
    assert!(comparison::load_report(&t.path().join("alias")).is_err());
    symlink(t.path(), t.path().join("parent-alias")).unwrap();
    assert!(comparison::load_report(&t.path().join("parent-alias/suite")).is_err());
    let (t, root) = fixture();
    let journal = root.join("sources/copy/run-copy/journal.json");
    fs::rename(&journal, t.path().join("journal.json")).unwrap();
    symlink(t.path().join("journal.json"), journal).unwrap();
    assert!(comparison::cleanup(&root).is_err());
}
#[test]
fn oversized_envelopes_and_sources_fail_before_json_decode() {
    for source in [false, true] {
        let (_t, root) = fixture();
        let path = if source {
            root.join("sources/copy/run-copy/report.json")
        } else {
            root.join("comparison.json")
        };
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(64 * 1024 * 1024 + 1)
            .unwrap();
        let error = comparison::load_report(&root).unwrap_err();
        assert!(format!("{error:#}").contains("exceeds size limit"));
    }
}
#[test]
fn unfinished_cancelled_and_source_error_evidence_survive_cached_verdicts() {
    for mode in 0..4 {
        let (_t, root) = fixture();
        edit(&root, |v| match mode {
            0 => v["finished"] = false.into(),
            1 => v["cancelled"] = true.into(),
            2 => v["sources"][1]["error"] = "source failed to save".into(),
            _ => v["sources"] = serde_json::json!([]),
        });
        assert_eq!(
            comparison::load_report(&root).unwrap().outcome,
            if mode == 1 {
                RunOutcome::Cancelled
            } else {
                RunOutcome::Incomplete
            }
        );
        comparison::cleanup(&root).unwrap();
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(root.join("sources/copy/run-copy/journal.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
