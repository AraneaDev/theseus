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
        VALID.replace("schema_version = 1", "schema_version = 2"),
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
