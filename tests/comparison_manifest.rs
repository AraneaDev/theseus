use theseus::comparison::{SuiteManifest, load, validate_manifest};
const SUITE: &str = "schema_version=1\nname='methods'\nbaseline='copy'\nprofiles=['user']\nscenarios=['clean']\nstages=['configured']\n[[variants]]\nid='copy'\nrecipe='recipes/a.toml'\n[[variants]]\nid='archive'\nrecipe='recipes/b.toml'\n[[scopes]]\nid='application'\nprofile='user'\npath='/home/theseus'\n";
fn fixture(text: &str, recipe: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("recipes/target")).unwrap();
    for name in ["a.toml", "b.toml"] {
        std::fs::write(d.path().join("recipes").join(name), recipe).unwrap();
    }
    let p = d.path().join("comparison.toml");
    std::fs::write(&p, text).unwrap();
    (d, p)
}
const RECIPE: &str = include_str!("fixtures/recipes/v2_clean.toml");
#[test]
fn relative_recipes_resolve_at_manifest_parent_and_validate_before_running() {
    let (d, p) = fixture(SUITE, RECIPE);
    let suite = load(&p).unwrap();
    assert_eq!(suite.project_root, d.path());
    assert_eq!(suite.variants.len(), 2);
    assert_eq!(
        suite.variants[0].recipe.path,
        d.path().join("recipes/a.toml")
    );
}
#[test]
fn schema_unknown_fields_ids_selections_and_scope_paths_are_validated_offline() {
    for text in [
        SUITE.replace("schema_version=1", "schema_version=2"),
        format!("unexpected=1\n{SUITE}"),
        SUITE.replace("id='archive'", "id='copy'"),
        SUITE.replace("id='archive'", "id='../escape'"),
        SUITE.replace("baseline='copy'", "baseline='other'"),
        SUITE.replace("profiles=['user']", "profiles=[]"),
        SUITE.replace("profiles=['user']", "profiles=['user','user']"),
        SUITE.replace("scenarios=['clean']", "scenarios=['clean','clean']"),
        SUITE.replace("stages=['configured']", "stages=[]"),
        SUITE.replace(
            "stages=['configured']",
            "stages=['configured','configured']",
        ),
        SUITE.replace("path='/home/theseus'", "path='/'"),
        SUITE.replace("path='/home/theseus'", "path='/home/../app'"),
        SUITE.replace("profile='user'", "profile='system'"),
        SUITE.replace(
            "recipe='recipes/b.toml'",
            "recipe='recipes/b.toml'\nunknown=1",
        ),
    ] {
        let result = toml::from_str::<SuiteManifest>(&text)
            .map_err(anyhow::Error::from)
            .and_then(|m| validate_manifest(&m));
        assert!(result.is_err(), "accepted {text}");
    }
}
#[test]
fn recipe_matrix_and_observation_coverage_are_checked() {
    for (suite, recipe) in [
        (
            SUITE.replace("profiles=['user']", "profiles=['user','system']"),
            RECIPE.to_string(),
        ),
        (
            SUITE.replace("scenarios=['clean']", "scenarios=['upgrade']"),
            RECIPE.to_string(),
        ),
        (
            SUITE.replace("path='/home/theseus'", "path='/outside'"),
            RECIPE.to_string(),
        ),
        (
            SUITE.to_string(),
            RECIPE.replace(
                "observe = [\"/home/theseus\"]",
                "observe = [\"/home/theseus\", \"/home/theseus/sub\"]",
            ),
        ),
    ] {
        let (_d, p) = fixture(&suite, &recipe);
        assert!(load(&p).is_err());
    }
}
#[test]
fn dimensions_and_byte_limit_are_bounded() {
    let base: SuiteManifest = toml::from_str(SUITE).unwrap();
    for n in [0, 1, 17] {
        let mut m = base.clone();
        m.variants = (0..n)
            .map(|i| {
                let mut v = base.variants[0].clone();
                v.id = format!("v{i}");
                v
            })
            .collect();
        m.baseline = "v0".into();
        assert!(validate_manifest(&m).is_err());
    }
    for n in [0, 33] {
        let mut m = base.clone();
        m.scopes = (0..n)
            .map(|i| {
                let mut s = base.scopes[0].clone();
                s.id = format!("s{i}");
                s
            })
            .collect();
        assert!(validate_manifest(&m).is_err());
        let mut m = base.clone();
        m.stages = (0..n).map(|i| format!("stage{i}")).collect();
        assert!(validate_manifest(&m).is_err());
    }
    for name in ["".to_string(), "x".repeat(129)] {
        let mut m = base.clone();
        m.name = name;
        assert!(validate_manifest(&m).is_err());
    }
    let mut m = base.clone();
    m.variants = (0..16)
        .map(|i| {
            let mut v = base.variants[0].clone();
            v.id = format!("v{i}");
            v
        })
        .collect();
    m.baseline = "v0".into();
    m.scopes = (0..32)
        .map(|i| {
            let mut s = base.scopes[0].clone();
            s.id = format!("s{i}");
            s
        })
        .collect();
    m.stages = (0..32).map(|i| format!("stage{i}")).collect();
    assert!(validate_manifest(&m).is_err());
    let (_d, p) = fixture(&format!("{}\n#{}", SUITE, "x".repeat(1_048_576)), RECIPE);
    assert!(load(&p).is_err());
}
#[test]
fn plugin_checkpoint_names_and_custom_stages_keep_existing_matrix_budget() {
    let mut manifest:SuiteManifest=toml::from_str(&SUITE.replace("scenarios=['clean']","scenarios=['plugin_lifecycle']").replace("stages=['configured']","stages=['base_installed','target_installed','plugin_disabled','plugin_enabled','uninstalled','custom-stage']")).unwrap();
    validate_manifest(&mut manifest).unwrap();
    assert_eq!(manifest.stages.len(), 6);
    manifest.scenarios = vec![
        theseus::model::ScenarioKind::Clean,
        theseus::model::ScenarioKind::Upgrade,
        theseus::model::ScenarioKind::Reinstall,
        theseus::model::ScenarioKind::InterruptedInstall,
        theseus::model::ScenarioKind::InterruptedUpgrade,
        theseus::model::ScenarioKind::Downgrade,
        theseus::model::ScenarioKind::Rollback,
        theseus::model::ScenarioKind::InterruptedRollback,
        theseus::model::ScenarioKind::PluginLifecycle,
    ];
    assert!(validate_manifest(&mut manifest).is_err());
}
