use std::fs;
use theseus::model::{ProfileName, ScenarioKind, StepName};

fn load_text(text: &str) -> anyhow::Result<theseus::recipe::ValidatedRecipe> {
    let dir = tempfile::tempdir()?;
    fs::create_dir(dir.path().join("base"))?;
    fs::create_dir(dir.path().join("target"))?;
    fs::write(dir.path().join("target/install.sh"), "exit 0")?;
    fs::write(dir.path().join("theseus.toml"), text)?;
    theseus::recipe::load(&dir.path().join("theseus.toml"))
}
const VALID: &str = include_str!("fixtures/recipes/valid.toml");

#[test]
fn valid_recipe_has_explicit_profiles_defaults_and_selection() {
    let validated = load_text(VALID).unwrap();
    assert_eq!(validated.recipe.profiles.len(), 2);
    assert_eq!(
        validated.recipe.profiles[&ProfileName::User].steps[&StepName::Configure].timeout_seconds,
        120
    );
    assert_eq!(validated.recipe.network, theseus::recipe::Network::None);
    assert_eq!(validated.recipe.limits.max_entries, 100_000);
    assert_eq!(validated.recipe.limits.max_content_bytes, 1_073_741_824);
    validated
        .validate_selection(
            &[ProfileName::User, ProfileName::System],
            &[ScenarioKind::Clean, ScenarioKind::Upgrade],
        )
        .unwrap();
    assert_eq!(validated.digest.len(), 64);
}

#[test]
fn rejects_unknown_fields_at_every_level() {
    for text in [
        format!("typo = true\n{VALID}"),
        VALID.replace("base = \"base\"", "base = \"base\"\ntypo = true"),
        VALID.replace(
            "script = \"sample-cli configure\"",
            "script = \"sample-cli configure\"\ntypo = true",
        ),
        VALID.replace("kind = \"absent\"", "kind = \"absent\"\ntypo = true"),
        VALID.replace(
            "[profiles.user.steps.install_base]",
            "[profiles.user]\ntypo = true\n[profiles.user.steps.install_base]",
        ),
    ] {
        assert!(load_text(&text).is_err(), "accepted {text}");
    }
}

#[test]
fn rejects_invalid_schema_steps_timeouts_and_duplicate_ids() {
    for text in [
        VALID.replace("schema_version = 1", "schema_version = 999"),
        VALID.replace("install_base]", "nonsense]"),
        VALID.replace(
            "sample-cli configure\"",
            "sample-cli configure\"\ntimeout_seconds = 0",
        ),
        VALID.replace(
            "sample-cli configure\"",
            "sample-cli configure\"\ntimeout_seconds = 3601",
        ),
        VALID.replace("id = \"target-version\"", "id = \"binary-removed\""),
    ] {
        assert!(load_text(&text).is_err());
    }
}

#[test]
fn unsafe_and_conflicting_roots_are_rejected() {
    for root in [
        "/",
        "relative",
        "/proc",
        "/sys/kernel",
        "/dev",
        "/usr/../etc",
        "/home/theseus/.config",
    ] {
        assert!(
            load_text(&VALID.replace("/usr/local/bin", root)).is_err(),
            "accepted {root}"
        );
    }
    let normalized =
        load_text(&VALID.replace("/usr/local/bin\"]", "/usr//local/./bin/\"]")).unwrap();
    assert_eq!(normalized.recipe.observe[1], "/usr/local/bin");
}

#[test]
fn assertions_must_be_observed_and_preservation_follows_configuration() {
    for text in [
        VALID.replace(
            "path = \"/usr/local/bin/sample-cli\"",
            "path = \"/etc/sample-cli\"",
        ),
        VALID
            .replace("kind = \"absent\"", "kind = \"preserved\"")
            .replace(
                "checkpoint = \"uninstalled\"",
                "checkpoint = \"base_installed\"",
            ),
        VALID.replace("checkpoint = \"uninstalled\"", "checkpoint = \"bogus\""),
    ] {
        assert!(load_text(&text).is_err());
    }
}

#[test]
fn selected_scenarios_require_their_steps_without_blocking_offline_preview() {
    let text = VALID.replace(
        "[profiles.user.steps.install_base]\nscript = \"sh /inputs/base/install.sh user\"\n",
        "",
    );
    let validated = load_text(&text).unwrap();
    validated
        .validate_selection(&[ProfileName::User], &[ScenarioKind::Clean])
        .unwrap();
    assert!(
        validated
            .validate_selection(&[ProfileName::User], &[ScenarioKind::Upgrade])
            .is_err()
    );
    let text = VALID.replace("base = \"base\"\n", "");
    let validated = load_text(&text).unwrap();
    assert!(
        validated
            .validate_selection(&[ProfileName::User], &[ScenarioKind::Upgrade])
            .is_err()
    );
    assert!(
        validated
            .validate_selection(&[], &[ScenarioKind::Clean])
            .is_err()
    );
}

#[test]
fn environment_forwarding_is_not_read_from_host_during_validation() {
    let text = VALID.replace(
        "script = \"sample-cli configure\"",
        "script = \"sample-cli configure\"\npass_env = [\"THESEUS_NOT_SET_TEST_VARIABLE\"]",
    );
    load_text(&text).unwrap();
}

#[test]
fn artifacts_resolve_in_project_and_cannot_escape_through_symlinks_or_special_files() {
    use std::os::unix::{fs::symlink, net::UnixListener};
    let dir = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("base")).unwrap();
    fs::create_dir(dir.path().join("target")).unwrap();
    fs::write(dir.path().join("theseus.toml"), VALID).unwrap();
    fs::write(external.path().join("secret"), "secret").unwrap();
    symlink(
        external.path().join("secret"),
        dir.path().join("target/leak"),
    )
    .unwrap();
    assert!(theseus::recipe::load(&dir.path().join("theseus.toml")).is_err());
    fs::remove_file(dir.path().join("target/leak")).unwrap();
    let _socket = UnixListener::bind(dir.path().join("target/socket")).unwrap();
    assert!(theseus::recipe::load(&dir.path().join("theseus.toml")).is_err());
}

#[test]
fn existing_recipe_defaults_remain_clean_and_upgrade() {
    let recipe =
        theseus::recipe::load(std::path::Path::new("tests/fixtures/recipes/valid.toml")).unwrap();
    assert_eq!(
        theseus::engine::RunOptions::all(&recipe).scenarios,
        vec![ScenarioKind::Clean, ScenarioKind::Upgrade]
    );
    assert!(
        !toml::to_string(&recipe.recipe)
            .unwrap()
            .contains("scenarios")
    );
}

#[test]
fn version_two_clean_only_requires_only_target_artifacts_and_clean_steps() {
    let recipe =
        theseus::recipe::load(std::path::Path::new("tests/fixtures/recipes/v2_clean.toml"))
            .unwrap();
    assert!(recipe.base_artifact.is_none());
    assert!(
        !recipe.recipe.profiles[&ProfileName::User]
            .steps
            .contains_key(&StepName::Upgrade)
    );
    assert_eq!(
        theseus::engine::RunOptions::all(&recipe).scenarios,
        vec![ScenarioKind::Clean]
    );
    recipe
        .validate_selection(&[ProfileName::User], &[ScenarioKind::Clean])
        .unwrap();
}

#[test]
fn scenario_declarations_follow_recipe_version_and_are_nonempty_and_unique() {
    for (text, expected) in [
        (
            format!("scenarios = [\"clean\"]\n{VALID}"),
            "schema version 2",
        ),
        (
            VALID.replace("schema_version = 1", "schema_version = 2"),
            "scenarios",
        ),
        (
            VALID.replace("schema_version = 1", "schema_version = 2\nscenarios = []"),
            "at least one scenario",
        ),
        (
            VALID.replace(
                "schema_version = 1",
                "schema_version = 2\nscenarios = [\"clean\", \"clean\"]",
            ),
            "duplicate scenario",
        ),
    ] {
        let error = load_text(&text).unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{error:#}");
    }
    assert!(
        load_text(&VALID.replace(
            "schema_version = 1",
            "schema_version = 2\nscenarios = [\"unknown\"]"
        ))
        .is_err()
    );
}

#[test]
fn version_two_declaration_order_controls_defaults_and_selection_rejects_undeclared_scenarios() {
    let recipe = load_text(&VALID.replace(
        "schema_version = 1",
        "schema_version = 2\nscenarios = [\"upgrade\", \"clean\"]",
    ))
    .unwrap();
    assert_eq!(
        theseus::engine::RunOptions::all(&recipe).scenarios,
        vec![ScenarioKind::Upgrade, ScenarioKind::Clean]
    );
    // Keep every upgrade prerequisite present so rejection proves the declaration is enforced.
    let clean_only = load_text(&VALID.replace(
        "schema_version = 1",
        "schema_version = 2\nscenarios = [\"clean\"]",
    ))
    .unwrap();
    let error = clean_only
        .validate_selection(&[ProfileName::User], &[ScenarioKind::Upgrade])
        .unwrap_err();
    assert!(error.to_string().contains("not declared"), "{error:#}");
}

const REINSTALL: &str = include_str!("fixtures/recipes/v2_reinstall.toml");
#[test]
fn reinstall_only_requires_repeat_and_stable_contract_for_each_selected_profile() {
    let valid = load_text(REINSTALL).expect("v2 reinstall-only is valid without base inputs");
    let selection = valid.declared_scenarios();
    valid
        .validate_selection(&[ProfileName::User], &selection)
        .unwrap();
    assert!(valid.base_artifact.is_none());
    for (text, expected) in [
        (
            REINSTALL.replace(
                "[profiles.user.steps.reinstall]\nscript = \"sh /inputs/target/install.sh user\"\n",
                "",
            ),
            "requires step reinstall",
        ),
        (
            REINSTALL.replace("kind = \"unchanged\"", "kind = \"present\""),
            "unchanged",
        ),
    ] {
        let recipe = load_text(&text).unwrap();
        let error = recipe
            .validate_selection(&[ProfileName::User], &recipe.declared_scenarios())
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }
    let text = format!(
        "{REINSTALL}\n[profiles.system.steps.install_target]\nscript='true'\n[profiles.system.steps.reinstall]\nscript='true'\n[profiles.system.steps.configure]\nscript='true'\n[profiles.system.steps.exercise]\nscript='true'\n[profiles.system.steps.uninstall]\nscript='true'\n"
    );
    let recipe = load_text(&text).unwrap();
    recipe
        .validate_selection(&[ProfileName::User], &recipe.declared_scenarios())
        .unwrap();
    assert!(
        recipe
            .validate_selection(&[ProfileName::System], &recipe.declared_scenarios())
            .unwrap_err()
            .to_string()
            .contains("unchanged")
    );
}
#[test]
fn reinstall_contracts_require_version_declaration_observation_and_checkpoint() {
    for (text, expected) in [
        (
            REINSTALL.replace("/home/theseus/config", "/etc/config"),
            "outside observed roots",
        ),
        (
            REINSTALL.replace(
                "checkpoint = \"reinstalled\"",
                "checkpoint = \"configured\"",
            ),
            "reinstalled",
        ),
        (
            REINSTALL.replace("scenarios = [\"reinstall\"]", "scenarios = [\"clean\"]"),
            "declare reinstall",
        ),
        (
            REINSTALL.replace(
                "schema_version = 2\nscenarios = [\"reinstall\"]",
                "schema_version = 1",
            ),
            "schema version 2",
        ),
        (
            REINSTALL
                .replace("kind = \"unchanged\"", "kind = \"present\"")
                .replace(
                    "checkpoint = \"reinstalled\"",
                    "checkpoint = \"configured\"",
                )
                .replace(
                    "schema_version = 2\nscenarios = [\"reinstall\"]",
                    "schema_version = 1",
                ),
            "schema version 2",
        ),
    ] {
        let error = load_text(&text).unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{error:#}");
    }
}
#[test]
fn reinstalled_preservation_uses_configured_reference_but_target_requires_upgrade() {
    let text = format!(
        "{REINSTALL}\n[[profiles.user.checks]]\nid='retained'\ncheckpoint='reinstalled'\nkind='preserved'\npath='/home/theseus/config'\n"
    );
    let recipe = load_text(&text).unwrap();
    recipe
        .validate_selection(&[ProfileName::User], &recipe.declared_scenarios())
        .unwrap();
    let recipe = load_text(&text.replace(
        "id='retained'\ncheckpoint='reinstalled'",
        "id='retained'\ncheckpoint='target_installed'",
    ))
    .unwrap();
    assert!(
        recipe
            .validate_selection(&[ProfileName::User], &recipe.declared_scenarios())
            .unwrap_err()
            .to_string()
            .contains("requires the upgrade scenario")
    );
}

#[test]
fn version_one_independently_rejects_each_reinstall_only_feature() {
    let ordinary = include_str!("fixtures/recipes/v2_clean.toml").replace(
        "schema_version = 2\nscenarios = [\"clean\"]",
        "schema_version = 1",
    );
    for addition in [
        "[profiles.user.steps.reinstall]\nscript='true'\n",
        "[[profiles.user.checks]]\nid='stable'\ncheckpoint='configured'\nkind='unchanged'\npath='/home/theseus/config'\n",
        "[[profiles.user.checks]]\nid='present'\ncheckpoint='reinstalled'\nkind='present'\npath='/home/theseus/config'\n",
    ] {
        let error = load_text(&format!("{ordinary}\n{addition}")).unwrap_err();
        assert!(
            format!("{error:#}").contains("schema version 2"),
            "{error:#}"
        );
    }
}

const INTERRUPTION: &str = include_str!("fixtures/recipes/v3_interruption.toml");
#[test]
fn v3_interruption_declares_order_and_scoped_minimum_contracts() {
    let recipe = load_text(INTERRUPTION).expect("schema3 interruption recipe loads");
    assert_eq!(
        recipe
            .declared_scenarios()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["interrupted_install", "interrupted_upgrade"]
    );
    recipe
        .validate_selection(&[ProfileName::User], &recipe.declared_scenarios())
        .unwrap();
    for (from, to, expected) in [
        (
            "kind = \"present\"\npath = \"/home/theseus/partial\"",
            "kind = \"command\"\nscript='true'\nexpected_exit=0",
            "filesystem",
        ),
        (
            "stdout_contains = \"2.0\"",
            "stdout_contains = \"\"",
            "Executable",
        ),
        (
            "kind = \"unchanged\"",
            "kind = \"present\"",
            "data contract",
        ),
        ("base = \"base\"\n", "", "artifacts.base"),
        (
            "[profiles.user.steps.recover_install]\nscript = \"recover-install\"\n",
            "",
            "recover_install",
        ),
    ] {
        let recipe = load_text(&INTERRUPTION.replace(from, to)).unwrap();
        let error = recipe
            .validate_selection(&[ProfileName::User], &recipe.declared_scenarios())
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }
}
#[test]
fn interruption_requires_v3_and_valid_reference_selection_and_private_env() {
    for version in [1, 2] {
        assert!(
            load_text(
                &INTERRUPTION.replace("schema_version = 3", &format!("schema_version = {version}"))
            )
            .is_err()
        );
    }
    for addition in [
        "env = { THESEUS_INTERRUPT_BARRIER = 'bad' }",
        "pass_env = ['THESEUS_INTERRUPT_BARRIER']",
    ] {
        assert!(
            load_text(&INTERRUPTION.replace(
                "script = \"recover-install\"",
                &format!("script = \"recover-install\"\n{addition}")
            ))
            .is_ok()
        );
        assert!(
            load_text(&INTERRUPTION.replace(
                "timeout_seconds = 9",
                &format!("timeout_seconds = 9\n{addition}")
            ))
            .is_err()
        );
    }
    let preserved = INTERRUPTION.replace("kind = \"unchanged\"", "kind = \"preserved\"");
    let recipe = load_text(&preserved).unwrap();
    assert!(
        recipe
            .validate_selection(&[ProfileName::User], &recipe.declared_scenarios())
            .is_err()
    );
    let selected = vec![recipe.declared_scenarios()[1]];
    recipe
        .validate_selection(&[ProfileName::User], &selected)
        .unwrap();
    assert!(
        load_text(&INTERRUPTION.replace(
            "scenarios = [\"interrupted_install\", \"interrupted_upgrade\"]",
            "scenarios = ['clean']"
        ))
        .is_err()
    );
}

#[test]
fn recovered_unchanged_cannot_satisfy_reinstall_minimum_and_new_features_are_independently_gated() {
    let text = INTERRUPTION.replace(
        "\"interrupted_upgrade\"]",
        "\"interrupted_upgrade\", \"reinstall\"]",
    );
    let recipe = load_text(&text).unwrap();
    let error = recipe
        .validate_selection(&[ProfileName::User], &[ScenarioKind::Reinstall])
        .unwrap_err();
    assert!(error.to_string().contains("unchanged"));
    let clean = include_str!("fixtures/recipes/v2_clean.toml");
    for addition in [
        "[profiles.user.steps.interrupt_install]\nscript='true'\n",
        "[profiles.user.steps.recover_upgrade]\nscript='true'\n",
        "[[profiles.user.checks]]\nid='partial'\ncheckpoint='interrupted'\nkind='present'\npath='/home/theseus/partial'\n",
        "[[profiles.user.checks]]\nid='target'\ncheckpoint='recovered'\nkind='present'\npath='/home/theseus/bin'\n",
    ] {
        for version in [1, 2] {
            let original = if version == 1 {
                clean.replace(
                    "schema_version = 2\nscenarios = [\"clean\"]",
                    "schema_version = 1",
                )
            } else {
                clean.into()
            };
            let error = load_text(&format!("{original}\n{addition}")).unwrap_err();
            assert!(format!("{error:#}").contains("schema version 3"));
        }
    }
}
#[test]
fn interruption_selection_ignores_contracts_at_unvisited_legacy_checkpoints() {
    let text = format!(
        "{INTERRUPTION}\n[[profiles.user.checks]]\nid='legacy-config'\ncheckpoint='target_installed'\nkind='preserved'\npath='/home/theseus/data'\n"
    );
    let recipe = load_text(&text).unwrap();
    recipe
        .validate_selection(&[ProfileName::User], &recipe.declared_scenarios())
        .unwrap();
}
