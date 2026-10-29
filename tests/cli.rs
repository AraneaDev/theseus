use std::process::Command;
fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_theseus"))
}
#[test]
fn init_refuses_to_overwrite_existing_recipe() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("theseus.toml"), "preserve me").unwrap();
    let output = cli().current_dir(temp.path()).arg("init").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("theseus.toml")).unwrap(),
        "preserve me"
    );
}
#[test]
fn starter_is_parseable_offline_and_intentionally_fails_until_configured() {
    let temp = tempfile::tempdir().unwrap();
    let output = cli().current_dir(temp.path()).arg("init").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let recipe = theseus::recipe::load(&temp.path().join("theseus.toml")).unwrap();
    recipe
        .validate_selection(
            &[
                theseus::model::ProfileName::User,
                theseus::model::ProfileName::System,
            ],
            &[
                theseus::model::ScenarioKind::Clean,
                theseus::model::ScenarioKind::Upgrade,
            ],
        )
        .unwrap();
    assert!(
        recipe
            .recipe
            .profiles
            .values()
            .all(|p| p.steps.values().all(|s| s.script.contains("exit 1")))
    );
    let output = cli()
        .current_dir(temp.path())
        .env("PATH", "/no-docker")
        .arg("validate")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
}
#[test]
fn report_json_works_without_docker() {
    let temp = tempfile::tempdir().unwrap();
    let store = theseus::report::create(&temp.path().join("run")).unwrap();
    store
        .save(&theseus::model::RunReport::new("offline"))
        .unwrap();
    let output = cli()
        .env("PATH", "/no-docker")
        .args(["report", store.root().to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: theseus::model::RunReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report.run_id, "offline");
}
#[test]
fn unknown_selections_and_interactive_launch_are_explicit_errors() {
    for args in [
        vec!["run", "--headless", "--profile", "bogus"],
        vec!["run", "--headless", "--scenario", "bogus"],
        vec![],
    ] {
        let output = cli().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn unavailable_profile_and_upgrade_inputs_fail_before_docker() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("input")).unwrap();
    let mut text = String::from(
        "schema_version=1\nname='offline selection'\nimage='ubuntu:24.04'\nobserve=['/home/theseus']\n[artifacts]\ntarget='input'\n",
    );
    for step in ["install_target", "configure", "exercise", "uninstall"] {
        text += &format!("[profiles.user.steps.{step}]\nscript='true'\n");
    }
    std::fs::write(temp.path().join("theseus.toml"), text).unwrap();
    for (args, expected) in [
        (
            vec!["run", "--headless", "--profile", "system"],
            "profile system is not defined",
        ),
        (
            vec!["run", "--headless", "--profile", "user"],
            "upgrade requires artifacts.base",
        ),
    ] {
        let output = cli()
            .current_dir(temp.path())
            .env("PATH", "/no-docker")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!temp.path().join(".theseus-runs").exists());
    }
}
