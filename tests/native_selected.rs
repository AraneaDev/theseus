#![cfg(feature = "native-catalog-test-support")]
//! Actual local catalog orchestration; fake provider receipts do not qualify VM reset.
#[path = "support/native_catalog_setup.rs"]
mod setup;
use setup::NativeCatalogFixture;
use theseus::{
    engine::{self, domain::*},
    model::{RunOutcome, ScenarioKind},
    native::model::*,
};

#[test]
fn selected_native_ordinary_scenarios_use_shared_sequence_and_original_authorities() {
    for (kind, expected) in [
        (
            ScenarioKind::Clean,
            vec![
                "baseline",
                "install_target",
                "target_installed",
                "configure",
                "configured",
                "exercise",
                "uninstall",
                "uninstalled",
            ],
        ),
        (
            ScenarioKind::Upgrade,
            vec![
                "baseline",
                "install_base",
                "base_installed",
                "configure",
                "configured",
                "upgrade",
                "target_installed",
                "exercise",
                "uninstall",
                "uninstalled",
            ],
        ),
        (
            ScenarioKind::Reinstall,
            vec![
                "baseline",
                "install_target",
                "target_installed",
                "configure",
                "configured",
                "exercise_before_reinstall",
                "reinstall",
                "reinstalled",
                "exercise_after_reinstall",
                "uninstall",
                "uninstalled",
            ],
        ),
    ] {
        let fixture = NativeCatalogFixture::new();
        let recipe = fixture.recipe(kind);
        let limits = recipe.recipe.limits.clone();
        let launch = fixture.launch();
        let mut events = vec![];
        let result = engine::run_selected(
            &SelectedRecipe::Native(recipe),
            SelectedRunOptions::Native(NativeRunOptions {
                output_root: fixture.root.path().join("reports"),
                scenarios: vec![kind],
                limits,
            }),
            Some(&launch),
            engine::CancellationToken::default(),
            |event| events.push(event),
        )
        .unwrap_or_else(|error| {
            let path = fixture.root.path().to_owned();
            let _ = std::fs::write(path.join("selected-error.txt"), format!("{error:#}"));
            if let Some(evidence) = std::env::var_os("THESEUS_NATIVE_SELECTED_EVIDENCE") {
                let evidence = std::path::PathBuf::from(evidence);
                if let Ok(entries) = std::fs::read_dir(path.join("reports")) {
                    for entry in entries.flatten() {
                        for filename in [REPORT_FILENAME, JOURNAL_FILENAME] {
                            let _ = std::fs::copy(
                                entry.path().join(filename),
                                evidence.join(format!("failed-{kind}-{filename}")),
                            );
                        }
                    }
                }
            }
            panic!("{kind}: {error:#}; fixture {}", path.display())
        });
        let StoredRun::Native(report) = result.report else {
            panic!("native entry returned legacy report")
        };
        assert_eq!(report.outcome, RunOutcome::Passed, "{kind}: {report:#?}");
        let scenario = &report.scenarios[0];
        assert_eq!(
            scenario
                .stages
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            expected,
            "{kind}"
        );
        for disposition in [
            &scenario.dispositions.local,
            &scenario.dispositions.remote,
            &scenario.dispositions.release,
            &scenario.dispositions.reset,
        ] {
            assert!(
                matches!(disposition, Disposition::Complete { .. }),
                "{kind}: {disposition:?}"
            );
        }
        let directory = result.report_path.parent().unwrap();
        assert!(!directory.join("report.json").exists());
        assert!(!directory.join("journal.json").exists());
        let journal =
            NativeJournal::decode(&std::fs::read(directory.join(JOURNAL_FILENAME)).unwrap())
                .unwrap();
        assert_eq!(
            journal.launch_fingerprint.as_deref(),
            Some(launch.view().fingerprint.as_str())
        );
        assert_eq!(journal.ownership.len(), 1);
        assert_eq!(
            journal
                .provider_calls
                .iter()
                .map(|call| call.invocation.action)
                .collect::<Vec<_>>(),
            [ProviderAction::Prepare, ProviderAction::Dispose]
        );
        for call in &journal.provider_calls {
            assert_eq!(
                call.receipt.as_ref().unwrap().invocation_digest,
                call.invocation.digest().unwrap()
            );
        }
        assert_eq!(
            journal.provider_calls[1].invocation.prior_resource,
            journal.ownership[0]
        );
        assert_eq!(journal.releases.len(), 1);
        assert!(matches!(
            journal.releases[0].completion,
            Disposition::Complete { .. }
        ));
        assert!(
            events
                .iter()
                .all(|event| event.report_path == result.report_path)
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.kind, engine::EventKind::StageFinished))
                .count(),
            expected.len()
        );
        assert!(
            scenario
                .stages
                .last()
                .unwrap()
                .native_files
                .iter()
                .all(|file| matches!(file.state, NativeFileState::AbsentSettled))
        );
    }
}

#[test]
fn failed_engine_publication_never_invokes_provider_or_acquires_guest() {
    let fixture = NativeCatalogFixture::new();
    let workspace_snapshot = || {
        let mut files = std::fs::read_dir(&fixture.workspace)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), std::fs::read(entry.path()).unwrap())
            })
            .collect::<Vec<_>>();
        files.sort();
        files
    };
    let workspace_before = workspace_snapshot();
    let recipe = fixture.recipe(ScenarioKind::Clean);
    let limits = recipe.recipe.limits.clone();
    let launch = fixture.launch();
    let result = engine::run_selected(
        &SelectedRecipe::Native(recipe),
        SelectedRunOptions::Native(NativeRunOptions {
            output_root: fixture.root.path().join("reports"),
            scenarios: vec![ScenarioKind::Clean],
            limits,
        }),
        Some(&launch),
        engine::CancellationToken::default(),
        |event| {
            if matches!(event.kind, engine::EventKind::Started) {
                let journal = event.report_path.parent().unwrap().join(JOURNAL_FILENAME);
                std::fs::remove_file(&journal).unwrap();
                std::fs::create_dir(journal).unwrap();
            }
        },
    );
    assert!(result.is_err());
    assert_eq!(
        std::fs::read_dir(fixture.root.path().join("provider"))
            .unwrap()
            .count(),
        0,
        "publication failure invoked a provider identity"
    );
    assert_eq!(
        workspace_snapshot(),
        workspace_before,
        "publication failure mutated guest workspace"
    );
}

#[test]
fn selected_lost_prepare_requires_matching_original_query_before_dispose() {
    use theseus_host::process::{ChildEnvironment, NativeCommand};
    for (fault, dispose) in [
        ("lost_prepare_query_none", false),
        ("lost_prepare_query_receipt", true),
    ] {
        let fixture = NativeCatalogFixture::new();
        let mut provider =
            NativeCommand::new(std::env::var_os("THESEUS_NATIVE_CATALOG_FIXTURE").unwrap());
        provider.args([
            "provider",
            fixture.root.path().join("provider").to_str().unwrap(),
            fixture
                .root
                .path()
                .join("provider-template.json")
                .to_str()
                .unwrap(),
        ]);
        provider.environment =
            ChildEnvironment::Replace(vec![("THESEUS_INERT_PROVIDER_FAULT".into(), fault.into())]);
        let launch = fixture
            .launch()
            .with_fixture_provider("inert_provider".into(), provider)
            .unwrap();
        let recipe = fixture.recipe(ScenarioKind::Clean);
        let limits = recipe.recipe.limits.clone();
        assert!(
            engine::run_selected(
                &SelectedRecipe::Native(recipe),
                SelectedRunOptions::Native(NativeRunOptions {
                    output_root: fixture.root.path().join("reports"),
                    scenarios: vec![ScenarioKind::Clean],
                    limits
                }),
                Some(&launch),
                engine::CancellationToken::default(),
                |_| {}
            )
            .is_err()
        );
        let run = std::fs::read_dir(fixture.root.path().join("reports"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let journal =
            NativeJournal::decode(&std::fs::read(run.join(JOURNAL_FILENAME)).unwrap()).unwrap();
        assert_eq!(
            journal.provider_calls.len(),
            if dispose { 2 } else { 1 },
            "{fault}: new disposition identity without positive original query"
        );
        assert_eq!(
            journal.provider_calls[0].invocation.action,
            ProviderAction::Prepare
        );
        assert!(journal.operations.is_empty());
        assert!(journal.releases.is_empty());
        if dispose {
            assert_eq!(
                journal.provider_calls[1].invocation.action,
                ProviderAction::Dispose
            );
            assert!(journal.provider_calls[0].receipt.is_some());
            assert!(journal.provider_calls[1].receipt.is_some());
        } else {
            assert!(journal.provider_calls[0].receipt.is_none());
            assert!(fixture.root.path().join("provider/owner").exists());
        }
    }
}
