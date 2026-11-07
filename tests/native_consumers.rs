use theseus::{
    engine::domain::StoredRun,
    model::RunOutcome,
    native::{
        model::*,
        storage,
        view::{self, Selector},
    },
    report,
};
fn native() -> NativeReport {
    NativeReport {
        kind: "theseus_native_report".into(),
        schema: 1,
        run_id: "run".into(),
        recipe_digest: "a".repeat(64),
        scenarios: vec![],
        identities: vec![Provenance::Unavailable {
            reason: "helper child identity not observed".into(),
        }],
        provider_claims: vec![],
        failures: vec![],
        outcome: RunOutcome::Incomplete,
    }
}
#[test]
fn native_retained_store_has_only_namespaced_files_and_offline_reader() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    store.save_native_report(&native()).unwrap();
    store
        .save_native_journal(&NativeJournal::new("run"))
        .unwrap();
    assert!(!store.root().join("report.json").exists());
    assert!(!store.root().join("journal.json").exists());
    assert!(matches!(
        storage::load_run(store.root()).unwrap(),
        StoredRun::Native(_)
    ));
    assert!(report::load(store.root()).is_err());
    assert!(report::load_journal(store.root()).is_err());
}
#[test]
fn public_native_cleanup_requires_explicit_authority_without_effects() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    store.save_native_report(&native()).unwrap();
    store
        .save_native_journal(&NativeJournal::new("run"))
        .unwrap();
    let report_before = std::fs::read(store.native_report_path()).unwrap();
    let journal_path = store.root().join(JOURNAL_FILENAME);
    let journal_before = std::fs::read(&journal_path).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_theseus"))
        .env("PATH", "/no-ssh-or-docker")
        .arg("cleanup")
        .arg(store.root())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--native-target"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("cleaned up"));
    assert_eq!(
        std::fs::read(store.native_report_path()).unwrap(),
        report_before
    );
    assert_eq!(std::fs::read(journal_path).unwrap(), journal_before);
    assert!(!store.root().join("native-intents").exists());
}
#[test]
fn mixed_families_refuse_read_write_and_legacy_cleanup_without_docker() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    store.save_native_report(&native()).unwrap();
    std::fs::write(
        store.root().join("journal.json"),
        br#"{"schema_version":1,"run_id":"run","resources":[],"cleanup_errors":[]}"#,
    )
    .unwrap();
    assert!(storage::load_run(store.root()).is_err());
    assert!(
        store
            .save_native_journal(&NativeJournal::new("run"))
            .is_err()
    );
    assert!(
        theseus::runner::Docker::default()
            .cleanup(store.root())
            .is_err()
    );
}
#[test]
fn missing_journal_and_native_bytes_renamed_legacy_never_clean_successfully() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    assert!(
        theseus::runner::Docker::default()
            .cleanup(store.root())
            .is_err()
    );
    std::fs::write(
        store.root().join("journal.json"),
        NativeJournal::new("run").encode().unwrap(),
    )
    .unwrap();
    assert!(
        theseus::runner::Docker::default()
            .cleanup(store.root())
            .is_err()
    );
}
#[test]
fn overlapping_native_and_legacy_journal_never_authorizes_empty_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    let bytes=br#"{"kind":"theseus_native_journal","schema":1,"schema_version":1,"run_id":"run","ownership":[],"operations":[],"provider_calls":[],"releases":[],"failures":[],"resources":[],"cleanup_errors":[]}"#;
    std::fs::write(store.root().join("journal.json"), bytes).unwrap();
    assert!(report::load_journal(store.root()).is_err());
    assert!(
        theseus::runner::Docker::default()
            .cleanup(store.root())
            .is_err()
    );
}
#[cfg(unix)]
#[test]
fn unsafe_reserved_name_is_not_absent_family() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    std::os::unix::fs::symlink("missing", store.root().join("native-journal.json")).unwrap();
    assert!(store.artifact_family().is_err());
    assert!(store.save_native_report(&native()).is_err());
}
#[test]
fn recursive_native_fields_and_duplicate_keys_refuse() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    let mut value = serde_json::to_value(native()).unwrap();
    value["identities"][0]["uid"] = serde_json::json!(0);
    std::fs::write(
        store.native_report_path(),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    assert!(store.load_native_report().is_err());
    let bytes = serde_json::to_string(&native())
        .unwrap()
        .replace("\"schema\":1", "\"schema\":1,\"schema\":1");
    std::fs::write(store.native_report_path(), bytes).unwrap();
    assert!(store.load_native_report().is_err());
    let mut bad = native();
    bad.identities = vec![Provenance::Unavailable {
        reason: String::new(),
    }];
    assert!(store.save_native_report(&bad).is_err());
}
#[test]
fn native_pages_are_bounded_and_preserve_provenance() {
    let report = native();
    assert!(view::evidence_page(&report, Selector::Identities, 0, 0).is_err());
    assert!(view::evidence_page(&report, Selector::Identities, 0, 65).is_err());
    assert!(view::evidence_page(&report, Selector::Identities, 2, 1).is_err());
    let page = view::evidence_page(&report, Selector::Identities, 0, 1).unwrap();
    assert_eq!(page.records[0]["provenance"], "unavailable");
    assert_eq!(page.next_offset, None);
}

fn recipe_json() -> serde_json::Value {
    let mut steps = serde_json::Map::new();
    for program in [
        CatalogProgram::InstallTarget,
        CatalogProgram::Configure,
        CatalogProgram::Exercise,
        CatalogProgram::Uninstall,
    ] {
        steps.insert(program.as_str().into(),serde_json::json!({"id":program.as_str(),"program":program,"expected_status":{"kind":"posix_exit","code":0},"stdout_literal":null}));
    }
    serde_json::json!({"kind":"theseus_native_recipe","schema":1,"os":"linux","manifest_digest":"1".repeat(64),"leaves":{"reader":"reader","app":"app","data":"data"},"base":{"length":1,"sha256":"2".repeat(64)},"target":{"length":1,"sha256":"3".repeat(64)},"scenarios":["clean"],"steps":steps,"checks":[{"id":"target-version","checkpoint":"target_installed","command":{"id":"target-version","program":"check_target","expected_status":{"kind":"posix_exit","code":0},"stdout_literal":"target"}},{"id":"configured-data","checkpoint":"configured","command":{"id":"configured-data","program":"check_data","expected_status":{"kind":"posix_exit","code":0},"stdout_literal":"data"}}],"limits":{"run_seconds":30,"inspection_seconds":30,"max_operations":128,"output_bytes":16384}})
}
#[test]
fn public_cli_native_preview_and_report_are_offline_and_profiles_refuse() {
    let temp = tempfile::tempdir().unwrap();
    let recipe = temp.path().join("native.json");
    std::fs::write(&recipe, serde_json::to_vec(&recipe_json()).unwrap()).unwrap();
    let bin = env!("CARGO_BIN_EXE_theseus");
    let output = std::process::Command::new(bin)
        .current_dir(temp.path())
        .env("PATH", "/no-ssh-or-docker")
        .args(["run", "--headless", "--preview", "--recipe"])
        .arg(&recipe)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("current helper account"));
    assert!(stdout.contains("unsupplied"));
    assert!(!temp.path().join(".theseus-runs").exists());
    let output = std::process::Command::new(bin)
        .current_dir(temp.path())
        .env("PATH", "/no-ssh-or-docker")
        .args([
            "run",
            "--headless",
            "--preview",
            "--profile",
            "user",
            "--recipe",
        ])
        .arg(&recipe)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let store = report::create(&temp.path().join("saved")).unwrap();
    store.save_native_report(&native()).unwrap();
    let output = std::process::Command::new(bin)
        .env("PATH", "/no-ssh-or-docker")
        .args(["report", "--json"])
        .arg(store.root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let decoded: NativeReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(decoded, native());
}
#[test]
fn public_native_tui_keeps_its_own_domain_and_surfaces_unknown_cleanup() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut report = native();
    report.scenarios.push(NativeScenario {
        id: "clean".into(),
        scenario: theseus::model::ScenarioKind::Clean,
        ownership_id: "original-owner".into(),
        stages: vec![],
        outcome: RunOutcome::Incomplete,
        dispositions: NativeDispositions {
            local: Disposition::Unknown {
                reason: "prior child join unproved".into(),
            },
            ..NativeDispositions::default()
        },
    });
    let mut app = theseus::tui::App::native_offline(report, std::path::PathBuf::new());
    assert!(app.report.is_none());
    assert!(app.recipe.is_none());
    assert_eq!(app.outcome(), RunOutcome::Incomplete);
    for _ in 0..3 {
        theseus::tui::input(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    }
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| theseus::tui::render(frame, &app))
        .unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("prior child join unproved"));
    assert!(text.contains("unknown"));
    assert!(!text.contains("container"));
}
#[test]
fn native_comparison_refuses_before_creating_a_suite_directory() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("native.json"),
        serde_json::to_vec(&recipe_json()).unwrap(),
    )
    .unwrap();
    let mut manifest: theseus::comparison::SuiteManifest =
        toml::from_str(include_str!("../examples/comparisons/methods.toml")).unwrap();
    for variant in &mut manifest.variants {
        variant.recipe = "native.json".into();
    }
    let suite = temp.path().canonicalize().unwrap().join("suite.toml");
    std::fs::write(&suite, toml::to_string(&manifest).unwrap()).unwrap();
    let result = theseus::comparison::load(&suite);
    assert!(format!("{:#}", result.unwrap_err()).contains("native comparisons are unsupported"));
    assert!(!temp.path().join(".theseus-comparisons").exists());
}

#[test]
fn command_and_file_pages_preserve_native_status_partial_capture_and_absence_domain() {
    let mut report = native();
    report.scenarios = vec![NativeScenario {
        id: "native-clean".into(),
        scenario: theseus::model::ScenarioKind::Clean,
        ownership_id: "original-owner".into(),
        outcome: RunOutcome::Incomplete,
        dispositions: NativeDispositions::default(),
        stages: vec![NativeStage {
            name: "reader-check".into(),
            step: None,
            checkpoint: Some(theseus::model::Checkpoint::TargetInstalled),
            outcome: theseus::model::StageOutcome::Incomplete,
            commands: vec![NativeCommandEvidence {
                command: CatalogCommand {
                    id: "reader".into(),
                    program: CatalogProgram::CheckTarget,
                    expected_status: NativeStatus::WindowsExit { code: 0 },
                    stdout_literal: Some("target".into()),
                },
                operation_id: "1".repeat(32),
                request_digest: "2".repeat(64),
                status: Some(NativeStatus::WindowsExit { code: u32::MAX }),
                stdout: "partial".into(),
                stdout_complete: false,
                stdout_utf8: true,
                stderr_complete: false,
                stdout_log: None,
                stderr_log: None,
                failure: Some("retained stream ended early".into()),
            }],
            checks: vec![],
            native_files: vec![
                NativeFileObservation {
                    request_id: "a".into(),
                    root_identity: "original".into(),
                    leaf: "reader".into(),
                    state: NativeFileState::Present {
                        length: 8192,
                        sha256: "3".repeat(64),
                    },
                },
                NativeFileObservation {
                    request_id: "b".into(),
                    root_identity: "original".into(),
                    leaf: "app".into(),
                    state: NativeFileState::AbsentSettled,
                },
                NativeFileObservation {
                    request_id: "c".into(),
                    root_identity: "original".into(),
                    leaf: "data".into(),
                    state: NativeFileState::Unknown {
                        reason: "mapping unproved".into(),
                    },
                },
            ],
            failures: vec![],
        }],
    }];
    let commands = view::evidence_page(&report, Selector::Commands, 0, 1).unwrap();
    let command = &commands.records[0]["command"];
    assert_eq!(command["status"]["kind"], "windows_exit");
    assert_eq!(command["status"]["code"], u32::MAX);
    assert_eq!(command["stdout_complete"], false);
    assert!(command.get("exit_code").is_none());
    let files = view::evidence_page(&report, Selector::NativeFiles, 0, 3).unwrap();
    assert_eq!(files.records[0]["native_file"]["state"]["length"], 8192);
    assert_eq!(
        files.records[1]["native_file"]["state"]["state"],
        "absent_settled"
    );
    assert_eq!(files.records[2]["native_file"]["state"]["state"], "unknown");
}

#[test]
fn native_recipe_rejects_impossible_posix_signal_status() {
    let mut value = recipe_json();
    value["steps"]["install_target"]["expected_status"] =
        serde_json::json!({"kind":"posix_signal","signal":0});
    assert!(
        theseus::native::recipe::ValidatedNativeRecipe::decode(
            &serde_json::to_vec(&value).unwrap(),
            "native.json".into()
        )
        .is_err()
    );
    value["steps"]["install_target"]["expected_status"] =
        serde_json::json!({"kind":"posix_signal","signal":256});
    assert!(
        theseus::native::recipe::ValidatedNativeRecipe::decode(
            &serde_json::to_vec(&value).unwrap(),
            "native.json".into()
        )
        .is_err()
    );
}
#[test]
fn native_intent_namespace_reopens_original_retained_directory_without_republishing() {
    use theseus_native_helper_store::IntentRecord;
    use theseus_native_protocol::v2::{BindingId, Digest, IntentId, WorkspaceMarker};
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("run");
    let store = report::create(&path).unwrap();
    store.save_native_report(&native()).unwrap();
    assert!(store.open_intent_store().is_err());
    assert!(!path.join("native-intents").exists());
    let record = IntentRecord {
        intent: IntentId::parse(&"1".repeat(32)).unwrap(),
        binding: BindingId::parse(&"2".repeat(32)).unwrap(),
        manifest_digest: Digest::parse(&"3".repeat(64)).unwrap(),
        workspace_marker: WorkspaceMarker::parse(&"4".repeat(32)).unwrap(),
    };
    let prepared = store
        .intent_store()
        .unwrap()
        .prepare(record.clone())
        .unwrap();
    assert_eq!(prepared.record(), &record);
    let original = temp.path().join("original");
    std::fs::rename(&path, &original).unwrap();
    std::fs::create_dir(&path).unwrap();
    let verified = store
        .open_intent_store()
        .unwrap()
        .verify_original(&record)
        .unwrap();
    assert_eq!(verified.record(), &record);
    assert!(!path.join("native-intents").exists());
    assert!(store.read_log("native-intents/metadata", 1).is_err());
    assert!(store.create_log("native-intents/metadata").is_err());
}

#[test]
fn native_retained_reader_rejects_impossible_actual_signal_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    let mut value = native();
    value.scenarios = vec![NativeScenario {
        id: "clean".into(),
        scenario: theseus::model::ScenarioKind::Clean,
        ownership_id: "owner".into(),
        outcome: RunOutcome::Incomplete,
        dispositions: NativeDispositions::default(),
        stages: vec![NativeStage {
            name: "invalid-status".into(),
            step: None,
            checkpoint: None,
            outcome: theseus::model::StageOutcome::Incomplete,
            commands: vec![NativeCommandEvidence {
                command: CatalogCommand {
                    id: "check".into(),
                    program: CatalogProgram::CheckTarget,
                    expected_status: NativeStatus::PosixExit { code: 0 },
                    stdout_literal: None,
                },
                operation_id: "1".repeat(32),
                request_digest: "2".repeat(64),
                status: Some(NativeStatus::PosixSignal { signal: 0 }),
                stdout: String::new(),
                stdout_complete: false,
                stdout_utf8: true,
                stderr_complete: false,
                stdout_log: None,
                stderr_log: None,
                failure: None,
            }],
            checks: vec![],
            native_files: vec![],
            failures: vec![],
        }],
    }];
    std::fs::write(
        store.native_report_path(),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    assert!(store.load_native_report().is_err());
}
