use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
use theseus::{comparison::*, model::*, report, snapshot::Snapshot};
pub fn manifest() -> SuiteManifest {
    SuiteManifest {
        schema_version: 1,
        name: "methods".into(),
        baseline: "copy".into(),
        profiles: vec![ProfileName::User],
        scenarios: vec![ScenarioKind::Clean],
        variants: vec![
            Variant {
                id: "copy".into(),
                recipe: "copy.toml".into(),
            },
            Variant {
                id: "archive".into(),
                recipe: "archive.toml".into(),
            },
        ],
        scopes: vec![Scope {
            id: "app".into(),
            profile: ProfileName::User,
            path: "/app".into(),
        }],
        stages: vec!["configured".into(), "missing_stage".into()],
    }
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn saved(parent: &Path) -> PathBuf {
    let root = parent.join("suite");
    fs::create_dir(&root).unwrap();
    let mut manifest = manifest();
    manifest.stages.truncate(1);
    let mut refs = vec![];
    for id in ["copy", "archive"] {
        let path = format!("sources/{id}/run-{id}");
        let store = report::create(&root.join(&path)).unwrap();
        let mut r = RunReport::new(format!("run-{id}"));
        r.recipe_digest = "a".repeat(64);
        r.selected_profiles = manifest.profiles.clone();
        r.selected_scenarios = manifest.scenarios.clone();
        r.finished_at = Some("done".into());
        r.image = "fixture:exact".into();
        r.image_id = Some(format!("sha256:{id}"));
        let mut s = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
        s.teardown = TeardownStatus::Complete;
        let mut stage = StageReport::new("configured");
        stage.outcome = if id == "archive" {
            StageOutcome::Failed
        } else {
            StageOutcome::Passed
        };
        stage.snapshots = vec![Snapshot::missing("/app")];
        s.stages.push(stage);
        r.scenarios.push(s);
        store.save(&r).unwrap();
        store
            .save_journal(&RunJournal {
                schema_version: 1,
                run_id: r.run_id,
                resources: vec![],
                cleanup_errors: vec![],
            })
            .unwrap();
        refs.push(SourceReference {
            variant_id: id.into(),
            path,
            report_digest: Some(digest(&fs::read(store.report_path()).unwrap())),
            error: None,
        });
    }
    let a = ComparisonArtifact {
        schema_version: 1,
        suite_id: "fixture".into(),
        manifest_digest: digest(&serde_json::to_vec(&manifest).unwrap()),
        manifest,
        recipe_digests: [
            ("copy".into(), "a".repeat(64)),
            ("archive".into(), "a".repeat(64)),
        ]
        .into(),
        sources: refs,
        report: ComparisonReport {
            sources: vec![],
            cells: vec![],
            outcome: RunOutcome::Passed,
            diagnostics: vec![],
        },
        cancelled: false,
        finished: true,
    };
    fs::write(
        root.join("comparison.json"),
        serde_json::to_vec(&a).unwrap(),
    )
    .unwrap();
    root
}
pub fn recipe_suite(parent: &Path) -> PathBuf {
    fs::create_dir(parent.join("target")).unwrap();
    let recipe = "schema_version=2\nname='fixture'\nimage='fixture:exact'\nscenarios=['clean']\nobserve=['/app']\n[artifacts]\ntarget='target'\n[profiles.user.steps.install_target]\nscript='true'\n[profiles.user.steps.configure]\nscript='true'\n[profiles.user.steps.exercise]\nscript='true'\n[profiles.user.steps.uninstall]\nscript='true'\n";
    for id in ["copy", "archive"] {
        fs::write(parent.join(format!("{id}.toml")), recipe).unwrap();
    }
    let path = parent.join("comparison.toml");
    fs::write(&path, toml::to_string(&manifest()).unwrap()).unwrap();
    path
}

pub fn add_unknown_stage(root: &Path) {
    let path = root.join("comparison.json");
    let mut a: ComparisonArtifact = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    a.manifest.stages.push("missing_stage".into());
    a.manifest_digest = digest(&serde_json::to_vec(&a.manifest).unwrap());
    fs::write(path, serde_json::to_vec(&a).unwrap()).unwrap();
}
