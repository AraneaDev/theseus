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
    #[cfg(unix)]
    use std::os::unix::{fs::symlink, net::UnixListener};
    #[cfg(windows)]
    use std::os::windows::fs::symlink_file as symlink;
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
    #[cfg(unix)]
    {
        let _socket = UnixListener::bind(dir.path().join("target/socket")).unwrap();
        assert!(theseus::recipe::load(&dir.path().join("theseus.toml")).is_err());
    }
    #[cfg(windows)]
    {
        fs::create_dir(dir.path().join("target/reparse-source")).unwrap();
        std::os::windows::fs::symlink_dir(
            external.path(),
            dir.path().join("target/reparse-source/escape"),
        )
        .unwrap();
        assert!(theseus::recipe::load(&dir.path().join("theseus.toml")).is_err());
    }
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

#[test]
fn downgrade_schema_four_recipe_loads_runtime_contract() {
    let temporary = tempfile::tempdir().unwrap();
    for name in ["base", "target"] {
        std::fs::create_dir(temporary.path().join(name)).unwrap();
    }
    let path = temporary.path().join("theseus.toml");
    std::fs::write(&path, include_str!("fixtures/recipes/v4_downgrade.toml")).unwrap();
    let recipe = theseus::recipe::load(&path).expect("explicit downgrade schema loads");
    recipe
        .validate_selection(
            &[theseus::model::ProfileName::User],
            &recipe.declared_scenarios(),
        )
        .unwrap();
    assert_eq!(recipe.declared_scenarios()[0].to_string(), "downgrade");
}

const DOWNGRADE: &str = include_str!("fixtures/recipes/v4_downgrade.toml");
#[test]
fn downgrade_rejects_invalid_contract_references_and_schema_features() {
    for text in [
        DOWNGRADE.replace("schema_version = 4", "schema_version = 3"),
        DOWNGRADE.replace("scenarios = [\"downgrade\"]", "scenarios = [\"clean\"]"),
        DOWNGRADE.replace(
            "data_checks = [\"older-data\"]",
            "data_checks = [\"unknown\"]",
        ),
        DOWNGRADE.replace(
            "data_checks = [\"older-data\"]",
            "data_checks = [\"older\"]",
        ),
        DOWNGRADE.replace(
            "checkpoint = \"downgraded\"\nkind = \"command\"",
            "checkpoint = \"configured\"\nkind = \"command\"",
        ),
        DOWNGRADE.replace(
            "stdout_contains = \"record survives\"",
            "stdout_contains = \" \"",
        ),
        DOWNGRADE.replace("stdout_contains = \"1.0\"", "stdout_contains = \"\""),
        DOWNGRADE.replace("stdout_contains = \"2.0\"", "stdout_contains = \"\""),
    ] {
        assert!(load_text(&text).is_err(), "accepted {text}");
    }
    for text in [
        DOWNGRADE.replace("base = \"base\"\n", ""),
        DOWNGRADE.replace(
            "[profiles.user.steps.downgrade]\nscript = \"sh /inputs/base/install.sh user\"\n",
            "",
        ),
        DOWNGRADE.replace(
            "[profiles.user.steps.exercise_downgraded]\nscript = \"exercise-older\"\n",
            "",
        ),
        DOWNGRADE.replace(
            "[profiles.user.downgrade]\nexpect = \"compatible\"\ndata_checks = [\"older-data\"]\n",
            "",
        ),
    ] {
        let recipe = load_text(&text).unwrap();
        assert!(
            recipe
                .validate_selection(&[ProfileName::User], &recipe.declared_scenarios())
                .is_err()
        );
    }
}
#[test]
fn downgrade_rejection_requires_distinct_observed_unchanged_roles() {
    let text = DOWNGRADE.replace("expect = \"compatible\"\ndata_checks = [\"older-data\"]", "expect = \"safe_rejection\"\nexpected_exit = 9\nstdout_contains = \"newer data\"\nbinary_checks = [\"binary\"]\ndata_checks = [\"data\"]");
    let mut text = text[..text
        .find("[[profiles.user.checks]]\nid = \"older\"")
        .unwrap()]
        .to_string();
    text.push_str("\n[[profiles.user.checks]]\nid = \"newer-still\"\ncheckpoint = \"downgrade_rejected\"\nkind = \"executable\"\nname = \"sample-cli\"\nresolved_path = \"/home/theseus/bin\"\nexpected_exit = 0\nstdout_contains = \"2.0\"\n[[profiles.user.checks]]\nid = \"binary\"\ncheckpoint = \"downgrade_rejected\"\nkind = \"unchanged\"\npath = \"/home/theseus/bin\"\n[[profiles.user.checks]]\nid = \"data\"\ncheckpoint = \"downgrade_rejected\"\nkind = \"unchanged\"\npath = \"/home/theseus/data\"\n");
    load_text(&text)
        .unwrap()
        .validate_selection(&[ProfileName::User], &[ScenarioKind::Downgrade])
        .unwrap();
    for broken in [
        text.replace("expected_exit = 9", "expected_exit = 126"),
        text.replace("stdout_contains = \"newer data\"", "stdout_contains = \"\""),
        text.replace("binary_checks = [\"binary\"]", "binary_checks = [\"data\"]"),
        text.replace(
            "path = \"/home/theseus/data\"",
            "path = \"/home/theseus/bin/data\"",
        ),
        text.replace("path = \"/home/theseus/data\"", "path = \"/home/theseus\""),
        text.replace("path = \"/home/theseus/data\"", "path = \"/outside\""),
        text.replace("kind = \"unchanged\"", "kind = \"preserved\""),
        text.replace(
            "checkpoint = \"downgrade_rejected\"\nkind = \"unchanged\"",
            "checkpoint = \"downgraded\"\nkind = \"unchanged\"",
        ),
    ] {
        assert!(load_text(&broken).is_err(), "accepted {broken}");
    }
}
#[test]
fn downgrade_checks_do_not_satisfy_reinstall_or_interruption_minima() {
    let text = DOWNGRADE.replace(
        "scenarios = [\"downgrade\"]",
        "scenarios = [\"downgrade\", \"reinstall\", \"interrupted_upgrade\"]",
    );
    let recipe = load_text(&text).unwrap();
    for kind in [ScenarioKind::Reinstall, ScenarioKind::InterruptedUpgrade] {
        let error = recipe
            .validate_selection(&[ProfileName::User], &[kind])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(if kind == ScenarioKind::Reinstall {
                "unchanged"
            } else {
                "interrupted filesystem"
            }),
            "{error}"
        );
    }
}

const ROLLBACK: &str = include_str!("fixtures/recipes/rollback.toml");

#[test]
fn ordinary_recipes_accept_rollback_prefixed_check_ids() {
    for version in 1..=6 {
        let text = VALID
            .replace(
                "schema_version = 1",
                &if version == 1 {
                    "schema_version = 1".into()
                } else {
                    format!("schema_version = {version}\nscenarios = ['clean']")
                },
            )
            .replace("binary-removed", "__theseus_rollback_user-retention");
        let recipe =
            load_text(&text).unwrap_or_else(|error| panic!("recipe schema{version}: {error:#}"));
        recipe
            .validate_selection(
                &[ProfileName::User, ProfileName::System],
                &[ScenarioKind::Clean],
            )
            .unwrap();
    }
}

#[test]
fn rollback_declared_recipes_reject_reserved_check_ids() {
    let text = ROLLBACK.replace("'reader'", "'__theseus_rollback_reader'");
    let error = load_text(&text).unwrap_err();
    assert!(
        format!("{error:#}").contains("reserved rollback check ID"),
        "{error:#}"
    );
}

#[test]
fn rollback_recipe_five_loads_and_selects_declared_profile() {
    let recipe = load_text(ROLLBACK).unwrap();
    let scenarios = recipe.declared_scenarios();
    assert_eq!(scenarios[0].to_string(), "rollback");
    recipe
        .validate_selection(&[ProfileName::User], &scenarios)
        .unwrap();
}

#[test]
fn rollback_rejects_missing_unbound_or_mismatched_runtime_and_other_roles() {
    for text in [
        ROLLBACK.replace("runtime_checks = ['runtime']\n", ""),
        ROLLBACK.replace(
            "runtime_checks = ['runtime']",
            "runtime_checks = ['reader']",
        ),
        ROLLBACK.replace("runtime_checks = ['runtime']", "runtime_checks = []"),
        ROLLBACK.replace(
            "\npath = '/workspace/tool'",
            "\npath = '/workspace/unrelated'",
        ),
        ROLLBACK.replacen(
            "stdout_contains = 'known good'",
            "stdout_contains = 'candidate'",
            1,
        ),
        ROLLBACK.replace("change_checks = ['changed']", "change_checks = ['binary']"),
        ROLLBACK.replace("data_checks = ['reader']", "data_checks = ['binary']"),
        ROLLBACK.replace("id = 'reader'", "id = '__theseus_rollback_reader'"),
        ROLLBACK.replace("source = '/workspace/data'", "source = '/unobserved/data'"),
        ROLLBACK.replace(
            "\npath = '/workspace/tool'",
            "\npath = '/workspace/data/tool'",
        ),
    ] {
        assert!(load_text(&text).is_err(), "accepted {text}");
    }
    for version in 1..=4 {
        assert!(
            load_text(
                &ROLLBACK.replace("schema_version = 5", &format!("schema_version = {version}"))
            )
            .is_err()
        );
    }
    let recipe = load_text(
        &ROLLBACK
            .replace("script = 'rollback'\n", "")
            .replace("[profiles.user.steps.rollback]\n", ""),
    )
    .unwrap();
    assert!(
        recipe
            .validate_selection(&[ProfileName::User], &[ScenarioKind::Rollback])
            .is_err()
    );
    assert!(
        load_text(&ROLLBACK.replace("scenarios = ['rollback']", "scenarios = ['clean']")).is_err()
    );
}

const INTERRUPTED_ROLLBACK: &str = include_str!("fixtures/recipes/interrupted_rollback.toml");
#[test]
fn interrupted_rollback_requires_schema_six_and_independent_known_good_restoration_roles() {
    let recipe = load_text(INTERRUPTED_ROLLBACK).unwrap();
    recipe
        .validate_selection(&[ProfileName::User], &[ScenarioKind::InterruptedRollback])
        .unwrap();
    for version in 1..=5 {
        assert!(
            load_text(
                &INTERRUPTED_ROLLBACK
                    .replace("schema_version = 6", &format!("schema_version = {version}"))
            )
            .is_err()
        );
    }
    for (from, to) in [
        ("base = 'base'\n", ""),
        ("steps.install_base", "steps.install_target"),
        ("steps.configure", "steps.upgrade"),
        ("steps.exercise]", "steps.reinstall]"),
        ("steps.interrupt_upgrade", "steps.recover_upgrade"),
        ("steps.rollback]", "steps.downgrade]"),
        ("steps.exercise_rolled_back", "steps.exercise_downgraded"),
        ("steps.uninstall", "steps.recover_install"),
        ("steps.backup]", "steps.install_target]"),
        ("steps.cleanup_backup", "steps.upgrade"),
        ("checkpoint = 'base_installed'", "checkpoint = 'configured'"),
        ("checkpoint = 'known_good'", "checkpoint = 'configured'"),
        (
            "checkpoint = 'interrupted'",
            "checkpoint = 'upgrade_failed'",
        ),
        ("binary_checks = ['binary']", "binary_checks = ['reader']"),
        (
            "runtime_checks = ['runtime']",
            "runtime_checks = ['reader']",
        ),
        ("data_checks = ['reader']", "data_checks = ['binary']"),
        ("change_checks = ['changed']", "change_checks = ['unknown']"),
        ("stdout_contains = 'known good'", "stdout_contains = ''"),
    ] {
        let changed = INTERRUPTED_ROLLBACK.replace(from, to);
        let accepted = load_text(&changed).and_then(|r| {
            r.validate_selection(&[ProfileName::User], &[ScenarioKind::InterruptedRollback])
        });
        assert!(accepted.is_err(), "{from} -> {to}");
    }
    let forbidden_failure = INTERRUPTED_ROLLBACK.replace("[profiles.user.interrupted_rollback.restoration]", "[profiles.user.interrupted_rollback.failure]\nexpected_exit=9\nstdout_contains='failure'\n[profiles.user.interrupted_rollback.restoration]");
    assert!(load_text(&forbidden_failure).is_err());
}

#[test]
fn interrupted_changed_checks_must_be_declared_policy_roles() {
    let extra = format!(
        "{INTERRUPTED_ROLLBACK}\n[[profiles.user.checks]]\nid='undeclared-change'\ncheckpoint='interrupted'\nkind='changed'\npath='/workspace/tool'\n"
    );
    assert!(load_text(&extra).is_err());
}

#[test]
fn binary_only_still_rejects_backup_steps_without_another_declared_restore_data_policy() {
    for (fixture, policy) in [
        (ROLLBACK, "rollback"),
        (INTERRUPTED_ROLLBACK, "interrupted_rollback"),
    ] {
        let mut value: toml::Value = toml::from_str(fixture).unwrap();
        value["profiles"]["user"][policy]["restoration"] =
            toml::from_str("mode='binary_only'").unwrap();
        let text = toml::to_string(&value).unwrap();
        assert!(toml::from_str::<theseus::recipe::Recipe>(&text).is_ok());
        let error = load_text(&text).unwrap_err().to_string();
        assert!(
            error.contains("binary_only has no backup steps"),
            "{policy}: {error}"
        );
    }
}
