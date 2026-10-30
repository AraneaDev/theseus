#[path = "support/plugin.rs"]
mod support;
use std::{fs, path::Path};
use theseus::{
    model::{ProfileName, ScenarioKind},
    recipe,
};

#[test]
fn guest_version_adapter_bounds_and_exit_status_are_checked_before_json() {
    let output = std::process::Command::new("python3")
        .args([
            "tests/support/plugin_prepare_test.py",
            "GuestVersionAdapter",
        ])
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn prepared_examples_load_all_scopes_without_host_paths_or_forwarded_environment() {
    let input = tempfile::tempdir().unwrap();
    let client = input.path().join("unexecutable-elf");
    fs::write(&client, b"\x7fELFdeterministic-never-executed").unwrap();
    for scope in ["user", "project", "local"] {
        let staged = support::stage_plugin(&client, "2.1.312 (Claude Code)", scope);
        let path = staged.path().join("prepared/theseus.toml");
        let r = recipe::load(&path).unwrap();
        assert_eq!(r.recipe.network, recipe::Network::None);
        assert_eq!(r.declared_scenarios(), vec![ScenarioKind::PluginLifecycle]);
        assert_eq!(r.recipe.profiles.len(), 1);
        let profile = &r.recipe.profiles[&ProfileName::User];
        assert_eq!(profile.plugin.as_ref().unwrap().scope.as_str(), scope);
        assert!(profile.steps.values().all(|s| s.pass_env.is_empty()));
        assert!(
            !fs::read_to_string(&path)
                .unwrap()
                .contains(input.path().to_str().unwrap())
        );
        assert!(
            Path::new(&r.target_artifact)
                .join("market-v2/plugins/theseus-probe/hooks/echo.sh")
                .is_file()
        );
    }
}
