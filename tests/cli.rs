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

#[test]
fn non_terminal_default_launch_explains_headless_and_emits_no_escapes() {
    let output = cli().output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("requires a terminal"));
    assert!(error.contains("--headless"));
    assert!(!output.stdout.contains(&0x1b));
    assert!(!output.stderr.contains(&0x1b));
}

#[test]
fn frozen_schema_one_and_schema_two_reports_browse_offline_and_old_journals_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("run");
    let store = theseus::report::create(&root).unwrap();
    for version in [1, 2] {
        let text = include_str!("fixtures/reports/schema1-report.json").replace(
            "\"schema_version\": 1",
            &format!("\"schema_version\": {version}"),
        );
        std::fs::write(store.report_path(), text).unwrap();
        let output = cli()
            .env("PATH", "/no-docker")
            .args(["report", root.to_str().unwrap(), "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["schema_version"], version);
        assert_eq!(
            report["scenarios"][0]["stages"][0]["checks"][0]["id"],
            "binary-removed"
        );
    }
    std::fs::write(
        root.join("journal.json"),
        include_bytes!("fixtures/reports/schema1-journal.json"),
    )
    .unwrap();
    let before = std::fs::read(store.report_path()).unwrap();
    for _ in 0..2 {
        let output = cli()
            .env("PATH", "/no-docker")
            .args(["cleanup", root.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(theseus::report::load_journal(&root).unwrap().resources[0].removed);
    assert_eq!(std::fs::read(store.report_path()).unwrap(), before);
}

#[test]
fn version_two_cli_all_uses_declarations_and_rejects_undeclared_upgrade_before_docker() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("target")).unwrap();
    std::fs::write(
        temp.path().join("theseus.toml"),
        include_str!("fixtures/recipes/v2_clean.toml"),
    )
    .unwrap();
    let output = cli()
        .current_dir(temp.path())
        .env("PATH", "/no-docker")
        .arg("validate")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = cli()
        .current_dir(temp.path())
        .env("PATH", "/no-docker")
        .args(["run", "--headless", "--scenario", "upgrade"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not declared"));
    assert!(!temp.path().join(".theseus-runs").exists());
    let output = cli()
        .current_dir(temp.path())
        .env("PATH", "/no-docker")
        .args(["run", "--headless", "--scenario", "all"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let runs = std::fs::read_dir(temp.path().join(".theseus-runs"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(runs.len(), 1);
    let report = theseus::report::load(&runs[0].path()).unwrap();
    assert_eq!(
        report.selected_scenarios,
        vec![theseus::model::ScenarioKind::Clean]
    );
    assert!(
        !report.errors.is_empty(),
        "missing Docker must be recorded as infrastructure failure"
    );
    assert_eq!(
        theseus::report::load_journal(&runs[0].path())
            .unwrap()
            .schema_version,
        1
    );
}

#[test]
fn reinstall_cli_rejects_undeclared_or_unavailable_before_creating_run() {
    for (text, expected) in [
        (
            include_str!("fixtures/recipes/v2_clean.toml").to_owned(),
            "not declared",
        ),
        (
            include_str!("fixtures/recipes/v2_reinstall.toml").replace(
                "[profiles.user.steps.reinstall]\nscript = \"sh /inputs/target/install.sh user\"\n",
                "",
            ),
            "requires step reinstall",
        ),
        (
            include_str!("fixtures/recipes/v2_reinstall.toml")
                .replace("kind = \"unchanged\"", "kind = \"present\""),
            "unchanged",
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("target")).unwrap();
        std::fs::write(temp.path().join("theseus.toml"), text).unwrap();
        let output = cli()
            .current_dir(temp.path())
            .env("PATH", "/no-docker")
            .args(["run", "--headless", "--scenario", "reinstall"])
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
#[test]
fn reinstall_cli_selects_declared_repeat_without_upgrade_inputs() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("target")).unwrap();
    std::fs::write(
        temp.path().join("theseus.toml"),
        include_str!("fixtures/recipes/v2_reinstall.toml"),
    )
    .unwrap();
    let output = cli()
        .current_dir(temp.path())
        .env("PATH", "/no-docker")
        .args(["run", "--headless", "--scenario", "reinstall"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let run = std::fs::read_dir(temp.path().join(".theseus-runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let report = theseus::report::load(&run.path()).unwrap();
    assert_eq!(
        report.selected_scenarios,
        [theseus::model::ScenarioKind::Reinstall]
    );
    assert!(report.scenarios.is_empty());
    assert!(!report.errors.is_empty());
}
