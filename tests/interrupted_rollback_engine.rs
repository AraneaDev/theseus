include!("support/parity/interrupted_rollback_engine.rs");

#[test]
fn both_profiles_and_policies_verify_interruption_before_restoring_original() {
    for profile in [ProfileName::User, ProfileName::System] {
        for binary in [false, true] {
            let (_temp, result, env) = run("", binary, profile, None);
            assert_eq!(
                result.report.outcome,
                RunOutcome::Passed,
                "{:#?}",
                result.report.scenarios[0]
            );
            let stages = &result.report.scenarios[0].stages;
            let start = stages
                .iter()
                .position(|s| s.name == "before_interrupt")
                .unwrap();
            assert_eq!(
                stages[start..start + 10]
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>(),
                [
                    "before_interrupt",
                    "interrupt_upgrade",
                    "restart_after_interrupt",
                    "interrupted",
                    "interrupted:after_checks",
                    "before_rollback",
                    "rollback",
                    "rolled_back",
                    "rolled_back:after_checks",
                    "exercise_rolled_back"
                ]
            );
            assert!(stages[start + 1].verified_interruption());
            assert!(stages[start + 1].snapshots.is_empty());
            assert!(stages[start + 2].snapshots.is_empty());
            assert_eq!(
                stages[start + 2].restart.as_ref().unwrap().container_id,
                "same-container"
            );
            assert_eq!(env.setups, 1);
            assert_eq!(env.restarts, 1);
            assert_eq!(
                env.calls.iter().filter(|r| r.phase == "configure").count(),
                1
            );
            assert!(
                !env.calls
                    .iter()
                    .any(|r| r.phase == "recover_upgrade" || r.phase == "attempt_upgrade")
            );
            for q in &env.calls {
                let runtime = q.phase.starts_with("check:")
                    || q.phase.starts_with("resolve:")
                    || q.phase.starts_with("exercise")
                    || q.phase == "configure";
                assert_eq!(
                    q.identity.uid,
                    if runtime || profile == ProfileName::User {
                        10001
                    } else {
                        0
                    }
                );
                assert_eq!(q.expected_exit, 0);
            }
            assert_eq!(
                theseus::report::load(&result.report_path).unwrap().outcome,
                RunOutcome::Passed
            );
        }
    }
}

#[test]
fn blocked_evidence_never_authorizes_rollback_and_cleanup_obeys_availability() {
    for (fault, expected, cleanup) in [
        ("no-mutation", RunOutcome::Failed, true),
        ("incomplete:16", RunOutcome::Incomplete, true),
        ("early", RunOutcome::Failed, true),
        ("unverified", RunOutcome::Incomplete, false),
        ("contradiction", RunOutcome::Incomplete, false),
        ("malformed-interrupt", RunOutcome::Incomplete, false),
        ("restart-error", RunOutcome::Incomplete, false),
        ("wrong-id", RunOutcome::Incomplete, false),
        ("unverified-restart", RunOutcome::Incomplete, false),
        ("cancel-stop", RunOutcome::Cancelled, false),
        ("cancel-restart", RunOutcome::Cancelled, false),
        ("attempt-corrupts-backup", RunOutcome::Failed, true),
        ("failure-check-corrupts", RunOutcome::Failed, true),
        ("backup-check-effect", RunOutcome::Failed, true),
        ("preexisting", RunOutcome::Failed, false),
    ] {
        let (_temp, result, env) = run(fault, false, ProfileName::User, None);
        assert_eq!(result.report.outcome, expected, "{fault}");
        assert!(
            !env.calls
                .iter()
                .any(|q| q.phase == "rollback" || q.phase == "exercise_rolled_back"),
            "{fault}"
        );
        assert_eq!(
            env.calls.iter().any(|q| q.phase == "cleanup_backup"),
            cleanup,
            "{fault}"
        );
        assert_eq!(
            theseus::report::load(&result.report_path).unwrap().outcome,
            expected,
            "{fault}"
        );
    }
}

#[test]
fn serialized_success_requires_each_stage_check_command_and_role_proof() {
    let (_temp, valid, _) = run("", false, ProfileName::System, None);
    assert_eq!(valid.report.outcome, RunOutcome::Passed);
    let original = serde_json::to_value(&valid.report).unwrap();
    let assert_blocked = |value: serde_json::Value, label: &str| {
        std::fs::write(
            &valid.report_path,
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
        let loaded = theseus::report::load(&valid.report_path).unwrap();
        assert!(
            matches!(loaded.outcome, RunOutcome::Incomplete | RunOutcome::Failed),
            "{label}: {}",
            loaded.outcome
        );
    };
    let stages = original["scenarios"][0]["stages"].as_array().unwrap();
    for (i, stage) in stages.iter().enumerate() {
        let mut value = original.clone();
        value["scenarios"][0]["stages"]
            .as_array_mut()
            .unwrap()
            .remove(i);
        assert_blocked(value, &format!("stage {i}"));
        for (field, list) in [
            ("checks", stage["checks"].as_array().unwrap()),
            ("commands", stage["commands"].as_array().unwrap()),
        ] {
            for j in 0..list.len() {
                let mut value = original.clone();
                value["scenarios"][0]["stages"][i][field]
                    .as_array_mut()
                    .unwrap()
                    .remove(j);
                assert_blocked(value, &format!("remove {field} {i}/{j}"));
            }
        }
    }
    for (stage_name, phase) in [
        ("known_good", "check:known"),
        ("base_installed", "check:base"),
        ("rolled_back", "check:runtime"),
        ("rolled_back", "check:reader"),
        ("interrupt_upgrade", "interrupt_upgrade"),
        ("rollback", "rollback"),
    ] {
        let i = stages.iter().position(|s| s["name"] == stage_name).unwrap();
        let commands = stages[i]["commands"].as_array().unwrap();
        let j = commands.iter().position(|c| c["phase"] == phase).unwrap();
        for (field, replacement) in [
            ("argv", serde_json::json!(["/bin/true"])),
            ("phase", serde_json::json!("unrelated")),
            (
                "identity",
                serde_json::json!({"uid":42,"gid":42,"home":"/wrong"}),
            ),
            (
                "expected_failure",
                serde_json::json!({"expected_exit":9,"stdout_contains":"failure","stdout_matched":true}),
            ),
            (
                "rejection",
                serde_json::json!({"expected_exit":9,"stdout_contains":"rejected","stdout_matched":true}),
            ),
        ] {
            let mut value = original.clone();
            value["scenarios"][0]["stages"][i]["commands"][j][field] = replacement;
            assert_blocked(value, &format!("{phase}/{field}"));
        }
    }
    for (stage_name, id) in [
        ("known_good", "known"),
        ("base_installed", "base"),
        ("rolled_back", "runtime"),
        ("rolled_back", "reader"),
    ] {
        let i = stages.iter().position(|s| s["name"] == stage_name).unwrap();
        let j = stages[i]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .position(|c| c["id"] == id)
            .unwrap();
        assert!(
            stages[i]["checks"][j]["command_expectation"].is_object(),
            "{id}: missing typed role proof"
        );
        for field in [
            "command_expectation",
            "stdout_contains",
            "stdout_matched",
            "stdout_match",
            "expected_exit",
        ] {
            let mut value = original.clone();
            if field == "command_expectation" {
                value["scenarios"][0]["stages"][i]["checks"][j]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
            } else {
                value["scenarios"][0]["stages"][i]["checks"][j]["command_expectation"][field] =
                    match field {
                        "stdout_contains" => serde_json::json!("wrong marker"),
                        "stdout_matched" => serde_json::json!(false),
                        "stdout_match" => serde_json::Value::Null,
                        _ => serde_json::json!(9),
                    };
            }
            assert_blocked(value, &format!("{id}/{field}"));
        }
        let command_id = stages[i]["checks"][j]["command_ids"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        let ci = stages[i]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .position(|c| &c["id"] == command_id)
            .unwrap();
        let mut value = original.clone();
        value["scenarios"][0]["stages"][i]["commands"][ci]["stdout"] = serde_json::Value::Null;
        assert_blocked(value, &format!("{id}/stdout"));
        for bytes in [0, 1] {
            let mut value = original.clone();
            value["scenarios"][0]["stages"][i]["commands"][ci]["stdout"]["bytes"] = bytes.into();
            assert_blocked(value, &format!("{id}/short stdout {bytes}"));
        }
        let mut value = original.clone();
        value["scenarios"][0]["stages"][i]["commands"][ci]["stdout"]["truncated"] = true.into();
        assert_blocked(value, &format!("{id}/truncated stdout"));
        if id == "runtime" || id == "base" {
            for (field, replacement) in [
                ("name", serde_json::json!("unrelated")),
                ("resolved_path", serde_json::json!("/workspace/unrelated")),
                ("args", serde_json::json!(["wrong"])),
            ] {
                let mut value = original.clone();
                value["scenarios"][0]["stages"][i]["checks"][j]["command_expectation"]["executable"]
                    [field] = replacement;
                assert_blocked(value, &format!("{id}/executable/{field}"));
            }
        }
    }
    for (stage_name, field, replacement) in [
        (
            "interrupt_upgrade",
            "readiness_verified",
            serde_json::json!(false),
        ),
        (
            "interrupt_upgrade",
            "stopped_verified",
            serde_json::json!(false),
        ),
        (
            "restart_after_interrupt",
            "running_verified",
            serde_json::json!(false),
        ),
        (
            "restart_after_interrupt",
            "container_id",
            serde_json::json!("other"),
        ),
    ] {
        let i = stages.iter().position(|s| s["name"] == stage_name).unwrap();
        let mut value = original.clone();
        if stage_name == "interrupt_upgrade" {
            value["scenarios"][0]["stages"][i]["commands"][0]["interruption"][field] = replacement;
        } else {
            value["scenarios"][0]["stages"][i]["restart"][field] = replacement;
        }
        assert_blocked(value, &format!("{stage_name}/{field}"));
    }
    for (stage_name, field) in [
        ("interrupted", "comparison"),
        ("before_rollback", "backup"),
        ("rolled_back", "backup"),
    ] {
        let i = stages.iter().position(|s| s["name"] == stage_name).unwrap();
        let j = stages[i]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .position(|c| c[field].is_object() && (field != "comparison" || c["id"] == "changed"))
            .unwrap();
        let mut value = original.clone();
        if field == "comparison" {
            value["scenarios"][0]["stages"][i]["checks"][j][field]["before_stage"] =
                "before_upgrade".into();
        } else {
            value["scenarios"][0]["stages"][i]["checks"][j][field]["stored_path"] =
                "/workspace/wrong".into();
        }
        assert_blocked(value, &format!("{stage_name}/{field}"));
    }
    let mut value = original.clone();
    value["scenarios"][0]["interrupted_rollback"]["restoration"]["scopes"][0]["stored"] =
        "wrong".into();
    assert_blocked(value, "scope mapping");
    for stage_name in ["interrupt_upgrade", "restart_after_interrupt"] {
        let i = stages.iter().position(|s| s["name"] == stage_name).unwrap();
        let mut value = original.clone();
        value["scenarios"][0]["stages"][i]["snapshots"] = stages[0]["snapshots"].clone();
        assert_blocked(value, &format!("{stage_name}/invented snapshots"));
    }
    let i = stages
        .iter()
        .position(|s| s["name"] == "before_interrupt")
        .unwrap();
    let configure = stages.iter().find(|s| s["name"] == "configure").unwrap();
    let mut value = original.clone();
    value["scenarios"][0]["stages"][i]["step"] = "configure".into();
    let mut command = configure["commands"][0].clone();
    command["phase"] = "before_interrupt".into();
    value["scenarios"][0]["stages"][i]["commands"] = serde_json::json!([command]);
    assert_blocked(value, "reference stage with forged step role");
    for stage_name in [
        "before_upgrade",
        "before_interrupt",
        "interrupted",
        "before_rollback",
        "rolled_back",
    ] {
        let i = stages.iter().position(|s| s["name"] == stage_name).unwrap();
        let mut value = original.clone();
        value["scenarios"][0]["stages"][i]["snapshots"][0]["completeness"] =
            serde_json::json!({"status":"incomplete","reason":"missing"});
        assert_blocked(value, &format!("{stage_name}/incomplete"));
    }
}

#[test]
fn cancellation_at_every_interrupted_rollback_boundary_stops_forward_commands() {
    let (_temp, valid, _) = run("", false, ProfileName::User, None);
    for stage in &valid.report.scenarios[0].stages {
        for kind in [EventKind::StageStarted, EventKind::StageFinished] {
            let (_temp, result, _) = run("", false, ProfileName::User, Some((kind, &stage.name)));
            assert_eq!(
                result.report.outcome,
                RunOutcome::Cancelled,
                "{kind:?} {}",
                stage.name
            );
            assert_eq!(
                theseus::report::load(&result.report_path).unwrap().outcome,
                RunOutcome::Cancelled
            );
        }
    }
}

#[test]
fn live_and_offline_interruption_views_require_complete_proof_for_verified_banner() {
    let (_temp, valid, _) = run("", false, ProfileName::User, None);
    let mut app = theseus::tui::App::offline(
        valid.report.clone(),
        valid.report_path.parent().unwrap().into(),
    );
    app.stage_index = valid.report.scenarios[0]
        .stages
        .iter()
        .position(|s| s.name == "interrupt_upgrade")
        .unwrap();
    let text = app.items().join("\n");
    assert!(text.contains("Interruption ContainerSigkill: readiness=true stopped=true"));
    assert!(text.contains("Interrupted rollback verified"));
    app.stage_index += 1;
    assert!(
        app.items()
            .join("\n")
            .contains("Restart: same-container running=true")
    );
    app.stage_index += 1;
    assert!(
        app.items()
            .join("\n")
            .contains("before_interrupt -> interrupted")
    );
    let mut stripped = valid.report.clone();
    let stop = stripped.scenarios[0]
        .stages
        .iter_mut()
        .find(|s| s.name == "interrupt_upgrade")
        .unwrap();
    stop.commands[0].interruption = None;
    app.active = true;
    app.set_report(stripped.clone(), valid.report_path.parent().unwrap().into());
    assert!(
        !app.items()
            .join("\n")
            .contains("Interrupted rollback verified")
    );
    std::fs::write(&valid.report_path, serde_json::to_vec(&stripped).unwrap()).unwrap();
    let report = theseus::report::load(&valid.report_path).unwrap();
    assert_eq!(report.outcome, RunOutcome::Incomplete);
    app.set_report(report, valid.report_path.parent().unwrap().into());
    assert!(
        !app.items()
            .join("\n")
            .contains("Interrupted rollback verified")
    );
}

#[test]
fn mixed_ordinary_and_interrupted_rollback_use_independent_policies_and_owned_containers() {
    let (_temp, result, env) = run("mixed", false, ProfileName::User, None);
    assert_eq!(
        result.report.outcome,
        RunOutcome::Passed,
        "{:#?}",
        result.report
    );
    assert_eq!(result.report.scenarios.len(), 2);
    let ordinary = &result.report.scenarios[0];
    let interrupted = &result.report.scenarios[1];
    assert_eq!(ordinary.container_id.as_deref(), Some("same-container"));
    assert_eq!(
        interrupted.container_id.as_deref(),
        Some("second-container")
    );
    assert!(ordinary.rollback.is_some() && ordinary.interrupted_rollback.is_none());
    assert!(interrupted.rollback.is_none() && interrupted.interrupted_rollback.is_some());
    assert_eq!(env.setups, 2);
    assert_eq!(env.restarts, 1);
    for (scenario, id, other) in [
        (ordinary, "ordinary-changed", "changed"),
        (interrupted, "changed", "ordinary-changed"),
    ] {
        assert!(
            scenario
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .any(|c| c.id == id && c.outcome == CheckOutcome::Passed)
        );
        assert!(
            !scenario
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .any(|c| c.id == other)
        );
    }
    assert_eq!(
        theseus::report::load(&result.report_path).unwrap().outcome,
        RunOutcome::Passed
    );
}

#[test]
fn unrelated_nonzero_command_checks_keep_their_declared_exit_expectation() {
    let (_temp, result, _) = run("nonzero-check", false, ProfileName::User, None);
    assert_eq!(result.report.outcome, RunOutcome::Passed);
    assert_eq!(
        theseus::report::load(&result.report_path).unwrap().outcome,
        RunOutcome::Passed
    );
}

#[test]
fn declared_restored_reader_can_use_a_distinct_script_and_marker() {
    let (_temp, result, _) = run("separate-reader", true, ProfileName::User, None);
    assert_eq!(result.report.outcome, RunOutcome::Passed);
    assert_eq!(
        theseus::report::load(&result.report_path).unwrap().outcome,
        RunOutcome::Passed
    );
}

#[test]
fn mixed_modes_keep_backup_steps_checks_and_authorization_with_restore_data_entry_only() {
    for (fault, binary_kind, restore_kind) in [
        (
            "mixed-ordinary-binary",
            ScenarioKind::Rollback,
            ScenarioKind::InterruptedRollback,
        ),
        (
            "mixed-interrupted-binary",
            ScenarioKind::InterruptedRollback,
            ScenarioKind::Rollback,
        ),
    ] {
        let (_temp, result, env) = run(fault, false, ProfileName::User, None);
        assert_eq!(
            result.report.outcome,
            RunOutcome::Passed,
            "{fault}: {:#?}",
            result.report
        );
        let binary = result
            .report
            .scenarios
            .iter()
            .find(|s| s.scenario == binary_kind)
            .unwrap();
        let restore = result
            .report
            .scenarios
            .iter()
            .find(|s| s.scenario == restore_kind)
            .unwrap();
        assert_ne!(binary.container_id, restore.container_id);
        assert!(!binary.stages.iter().any(|s| matches!(
            s.step,
            Some(StepName::Backup | StepName::CleanupBackup)
        ) || matches!(
            s.checkpoint,
            Some(Checkpoint::BackupCreated | Checkpoint::BackupRemoved)
        )));
        assert!(
            !binary
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .any(|c| c.backup.is_some()
                    || matches!(
                        c.checkpoint,
                        Checkpoint::BackupCreated | Checkpoint::BackupRemoved
                    ))
        );
        assert!(
            restore
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .any(|c| c.id == "backup-contract" && c.outcome == CheckOutcome::Passed)
        );
        assert!(
            restore
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .any(|c| c.id == "backup-removal-contract" && c.outcome == CheckOutcome::Passed)
        );
        assert_eq!(env.calls.iter().filter(|q| q.phase == "backup").count(), 1);
        assert_eq!(
            env.calls
                .iter()
                .filter(|q| q.phase == "cleanup_backup")
                .count(),
            1
        );
        assert_eq!(
            theseus::report::load(&result.report_path).unwrap().outcome,
            RunOutcome::Passed
        );
        let (_temp, cancelled, env) = run(
            fault,
            false,
            ProfileName::User,
            Some((EventKind::StageStarted, "baseline")),
        );
        assert_eq!(cancelled.report.outcome, RunOutcome::Cancelled);
        assert_eq!(cancelled.report.scenarios[0].scenario, binary_kind);
        assert!(
            !cancelled.report.scenarios[0]
                .stages
                .iter()
                .flat_map(|s| &s.checks)
                .any(|c| matches!(
                    c.checkpoint,
                    Checkpoint::BackupCreated | Checkpoint::BackupRemoved
                ))
        );
        assert!(
            !env.calls
                .iter()
                .any(|q| q.phase == "backup" || q.phase == "cleanup_backup")
        );
    }
}

#[test]
fn incomplete_checkpoint_observations_revoke_forward_authorization_permanently() {
    // Literal capture positions include command, checkpoint and after-check snapshots.
    // Each row independently states which forward commands were already authorized.
    let mut violations = vec![];
    for (capture, stage_name, backup, candidate, rollback, cleanup) in [
        (10, "known_good:after_checks", false, false, false, false),
        (14, "backup_created:after_checks", true, false, false, true),
        (17, "interrupted:after_checks", true, true, false, true),
        (21, "rolled_back:after_checks", true, true, true, true),
        (9, "known_good", false, false, false, false),
    ] {
        let (_temp, result, env) = run(
            &format!("incomplete:{capture}"),
            false,
            ProfileName::User,
            None,
        );
        assert_eq!(
            result.report.outcome,
            RunOutcome::Incomplete,
            "{stage_name}"
        );
        let scenario = &result.report.scenarios[0];
        let incomplete = scenario
            .stages
            .iter()
            .find(|s| s.name == stage_name)
            .unwrap();
        assert!(matches!(
            incomplete.snapshots[0].completeness,
            ObservationStatus::Incomplete { .. }
        ));
        // Later complete observations must never re-enable a failed prerequisite.
        let fresh = scenario
            .stages
            .iter()
            .find(|s| s.name == "before_rollback")
            .unwrap();
        assert!(!fresh.snapshots.is_empty());
        assert!(
            fresh
                .snapshots
                .iter()
                .all(|s| s.completeness == ObservationStatus::Complete)
        );
        for (phase, expected) in [
            ("backup", backup),
            ("interrupt_upgrade", candidate),
            ("rollback", rollback),
            ("exercise_rolled_back", false),
            ("cleanup_backup", cleanup),
            ("uninstall", true),
        ] {
            let executed = env.calls.iter().any(|q| q.phase == phase);
            if executed != expected {
                violations.push(format!("{stage_name} capture={capture}: {phase} executed={executed}, expected={expected}"));
            }
        }
        assert_eq!(
            theseus::report::load(&result.report_path).unwrap().outcome,
            RunOutcome::Incomplete,
            "{stage_name}"
        );
    }
    assert!(
        violations.is_empty(),
        "incomplete observation authorized forbidden commands: {violations:#?}"
    );
}

#[test]
fn every_required_capture_revokes_forward_authorization_permanently() {
    // Removing the central observation gate must expose actual forbidden commands.
    // Literal positions/order come from the independently reviewed lifecycle contract.
    let mut violations = vec![];
    for (capture, stage_name, backup, candidate, rollback, exercise, cleanup) in [
        (1, "baseline", false, false, false, false, false),
        (2, "install_base", false, false, false, false, false),
        (3, "base_installed", false, false, false, false, false),
        (
            4,
            "base_installed:after_checks",
            false,
            false,
            false,
            false,
            false,
        ),
        (5, "configure", false, false, false, false, false),
        (6, "configured", false, false, false, false, false),
        (
            7,
            "configured:after_checks",
            false,
            false,
            false,
            false,
            false,
        ),
        (
            8,
            "exercise_before_upgrade",
            false,
            false,
            false,
            false,
            false,
        ),
        (9, "known_good", false, false, false, false, false),
        (
            10,
            "known_good:after_checks",
            false,
            false,
            false,
            false,
            false,
        ),
        (11, "before_upgrade", false, false, false, false, false),
        (12, "backup", true, false, false, false, true),
        (13, "backup_created", true, false, false, false, true),
        (
            14,
            "backup_created:after_checks",
            true,
            false,
            false,
            false,
            true,
        ),
        (15, "before_interrupt", true, false, false, false, true),
        (16, "interrupted", true, true, false, false, true),
        (
            17,
            "interrupted:after_checks",
            true,
            true,
            false,
            false,
            true,
        ),
        (18, "before_rollback", true, true, false, false, true),
        (19, "rollback", true, true, true, false, true),
        (20, "rolled_back", true, true, true, false, true),
        (
            21,
            "rolled_back:after_checks",
            true,
            true,
            true,
            false,
            true,
        ),
        (22, "exercise_rolled_back", true, true, true, true, true),
        (23, "cleanup_backup", true, true, true, true, true),
        (24, "backup_removed", true, true, true, true, true),
        (
            25,
            "backup_removed:after_checks",
            true,
            true,
            true,
            true,
            true,
        ),
        (26, "uninstall", true, true, true, true, true),
        (27, "uninstalled", true, true, true, true, true),
        (28, "uninstalled:after_checks", true, true, true, true, true),
        (29, "final", true, true, true, true, true),
    ] {
        for fault in ["incomplete", "empty", "error"] {
            let (_temp, result, env) = run(
                &format!("{fault}:{capture}"),
                false,
                ProfileName::User,
                None,
            );
            let scenario = &result.report.scenarios[0];
            let target = scenario
                .stages
                .iter()
                .find(|s| s.name == stage_name)
                .unwrap();
            assert!(
                if fault == "empty" {
                    target.snapshots.is_empty()
                } else {
                    target
                        .snapshots
                        .iter()
                        .any(|s| matches!(s.completeness, ObservationStatus::Incomplete { .. }))
                },
                "{fault} {stage_name}: injection missed target"
            );
            for (phase, expected) in [
                ("resolve:base", capture > 3),
                ("check:base", capture > 3),
                ("check:known", capture > 9),
                ("backup", backup),
                ("interrupt_upgrade", candidate),
                ("rollback", rollback),
                ("exercise_rolled_back", exercise),
                ("cleanup_backup", cleanup),
                ("uninstall", true),
                ("configure", capture >= 5),
                ("exercise_before_upgrade", capture >= 8),
                ("install_base", capture >= 2),
            ] {
                let executed = env.calls.iter().any(|q| q.phase == phase);
                if executed != expected {
                    violations.push(format!("{fault} {stage_name} capture={capture}: {phase} executed={executed}, expected={expected}"));
                }
            }
            for (kind, outcome) in [
                ("live", result.report.outcome),
                (
                    "offline",
                    theseus::report::load(&result.report_path).unwrap().outcome,
                ),
            ] {
                if outcome != RunOutcome::Incomplete {
                    violations.push(format!(
                        "{fault} {stage_name}: {kind} outcome={outcome:?}, expected=Incomplete"
                    ));
                }
            }
            if stage_name != "final" {
                let later = scenario.stages.iter().find(|s| s.name == "final").unwrap();
                assert!(!later.snapshots.is_empty());
                assert!(
                    later
                        .snapshots
                        .iter()
                        .all(|s| s.completeness == ObservationStatus::Complete)
                );
            }
            assert_eq!(scenario.teardown, TeardownStatus::Complete);
        }
    }
    assert!(
        violations.is_empty(),
        "unverified capture authorized forbidden commands or lost Incomplete: {violations:#?}"
    );
}

#[test]
fn verified_observations_preserve_diagnostics_after_known_install_failure() {
    let (_temp, result, env) = run("install-fails", false, ProfileName::User, None);
    assert_eq!(result.report.outcome, RunOutcome::Failed);
    assert!(env.calls.iter().any(|q| q.phase == "check:base"));
    assert!(env.calls.iter().any(|q| q.phase == "uninstall"));
    assert!(!env.calls.iter().any(|q| matches!(
        q.phase.as_str(),
        "backup"
            | "attempt_upgrade"
            | "interrupt_upgrade"
            | "rollback"
            | "exercise_rolled_back"
            | "cleanup_backup"
    )));
}
