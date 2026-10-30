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

#[test]
fn interrupted_cli_selections_fail_unavailable_before_docker_and_preserve_declarations() {
    for scenario in ["interrupted-install", "interrupted-upgrade"] {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("target")).unwrap();
        std::fs::write(
            temp.path().join("theseus.toml"),
            include_str!("fixtures/recipes/v2_clean.toml"),
        )
        .unwrap();
        let out = cli()
            .current_dir(temp.path())
            .env("PATH", "/no-docker")
            .args(["run", "--headless", "--scenario", scenario])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("not declared"));
        assert!(!temp.path().join(".theseus-runs").exists());
    }
}
#[test]
fn interrupted_cli_selects_single_or_declared_matrix_without_docker() {
    for selection in ["all", "interrupted-install", "interrupted-upgrade"] {
        let temp = tempfile::tempdir().unwrap();
        for d in ["base", "target"] {
            std::fs::create_dir(temp.path().join(d)).unwrap();
        }
        let user = include_str!("fixtures/recipes/v3_interruption.toml");
        let system = user[user.find("[profiles.user.steps").unwrap()..]
            .replace("profiles.user", "profiles.system");
        std::fs::write(
            temp.path().join("theseus.toml"),
            format!("{user}\n{system}"),
        )
        .unwrap();
        let out = cli()
            .current_dir(temp.path())
            .env("PATH", "/no-docker")
            .args(["run", "--headless", "--scenario", selection])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        let root = std::fs::read_dir(temp.path().join(".theseus-runs"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        let report = theseus::report::load(&root.path()).unwrap();
        assert_eq!(report.selected_profiles.len(), 2);
        let names: Vec<_> = report
            .selected_scenarios
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            names,
            match selection {
                "all" => vec!["interrupted_install", "interrupted_upgrade"],
                "interrupted-install" => vec!["interrupted_install"],
                _ => vec!["interrupted_upgrade"],
            }
        );
        assert!(!report.errors.is_empty());
    }
}

#[test]
fn downgrade_cli_selection_is_explicit_and_validated_before_docker() {
    use clap::Parser;
    let parsed = theseus::cli::Cli::try_parse_from([
        "theseus",
        "run",
        "--headless",
        "--scenario",
        "downgrade",
    ])
    .unwrap();
    assert!(matches!(
        parsed.command,
        Some(theseus::cli::Action::Run {
            scenario: theseus::cli::ScenarioSelection::Downgrade,
            ..
        })
    ));
    let temp = tempfile::tempdir().unwrap();
    for name in ["base", "target"] {
        std::fs::create_dir(temp.path().join(name)).unwrap();
    }
    let text = include_str!("fixtures/recipes/v4_downgrade.toml").replace("base = \"base\"\n", "");
    std::fs::write(temp.path().join("theseus.toml"), text).unwrap();
    let out = cli()
        .current_dir(temp.path())
        .env("PATH", "/no-docker")
        .args(["run", "--headless", "--scenario", "downgrade"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("downgrade requires artifacts.base"));
}

#[test]
fn rollback_cli_selects_schema_five_and_rejects_missing_steps_before_docker() {
    let temp = tempfile::tempdir().unwrap();
    for p in ["base", "target"] {
        std::fs::create_dir(temp.path().join(p)).unwrap();
    }
    let path = temp.path().join("theseus.toml");
    let text = include_str!("fixtures/recipes/rollback.toml")
        .replace("[profiles.user.steps.rollback]\nscript = 'rollback'\n", "");
    std::fs::write(&path, text).unwrap();
    let output = cli()
        .current_dir(temp.path())
        .env("PATH", "/no-docker")
        .args(["run", "--headless", "--scenario", "rollback"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("requires step rollback"), "{error}");
    assert!(!temp.path().join(".theseus-runs").exists());
}

#[test]
fn interrupted_rollback_cli_selection_validates_offline_and_rejects_recovery_substitution() {
    let temp = tempfile::tempdir().unwrap();
    for name in ["base", "target"] {
        std::fs::create_dir(temp.path().join(name)).unwrap();
    }
    let text = include_str!("fixtures/recipes/interrupted_rollback.toml");
    let path = temp.path().join("theseus.toml");
    std::fs::write(&path, text).unwrap();
    let output = cli()
        .current_dir(temp.path())
        .env("PATH", "/no-docker")
        .args(["validate"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::write(
        &path,
        text.replace("steps.rollback]", "steps.recover_upgrade]"),
    )
    .unwrap();
    let output = cli()
        .current_dir(temp.path())
        .env("PATH", "/no-docker")
        .args([
            "run",
            "--headless",
            "--scenario",
            "interrupted-rollback",
            "--profile",
            "user",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires step rollback"));
}

#[test]
fn no_animation_is_global_for_every_command() {
    use clap::Parser;
    for args in [
        vec!["theseus", "--no-animation"],
        vec!["theseus", "run", "--no-animation"],
        vec!["theseus", "report", "saved", "--no-animation"],
        vec!["theseus", "validate", "--no-animation"],
        vec!["theseus", "init", "--no-animation"],
    ] {
        assert!(theseus::cli::Cli::try_parse_from(args).is_ok());
    }
}

#[test]
fn noninteractive_commands_do_not_draw_or_dwell_with_animation_default() {
    let temp = tempfile::tempdir().unwrap();
    let report = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/reports/schema1-report.json");
    let saved = temp.path().join("saved");
    std::fs::create_dir(&saved).unwrap();
    std::fs::copy(report, saved.join("report.json")).unwrap();
    let started = std::time::Instant::now();
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["init"],
        vec!["validate"],
        vec!["report", saved.to_str().unwrap(), "--json"],
        vec![
            "run",
            "--headless",
            "--profile",
            "user",
            "--scenario",
            "clean",
        ],
    ] {
        let output = cli()
            .current_dir(temp.path())
            .env("PATH", "/no-docker")
            .args(&args)
            .output()
            .unwrap();
        for bytes in [&output.stdout, &output.stderr] {
            assert!(!bytes.contains(&0x1b), "escapes for {args:?}");
            let text = String::from_utf8_lossy(bytes);
            assert!(
                !text.contains("T H E S E U S") && !text.contains("Entering the labyrinth"),
                "{text}"
            );
        }
        if args.first() == Some(&"run") {
            assert_eq!(output.status.code(), Some(2));
        } else {
            assert!(
                output.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "noninteractive path dwelled on animation"
    );
}
#[test]
fn plugin_lifecycle_is_a_public_cli_selection() {
    use clap::Parser;
    let cli = theseus::cli::Cli::try_parse_from([
        "theseus",
        "run",
        "--headless",
        "--profile",
        "user",
        "--scenario",
        "plugin-lifecycle",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Some(theseus::cli::Action::Run {
            scenario: theseus::cli::ScenarioSelection::PluginLifecycle,
            ..
        })
    ));
}
