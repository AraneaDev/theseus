#![allow(dead_code)]
//! Assertions over actual supplied-fixture evidence. This module creates no manager or tool handle.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::Value;
use std::path::Path;
use theseus::{
    engine::domain::StoredRun,
    model::{CheckOutcome, RunOutcome},
    native::integration::model::*,
    tui::{self, App},
};
use theseus_native_protocol::v2;

fn key(app: &mut App, code: KeyCode) {
    tui::input(app, KeyEvent::new(code, KeyModifiers::NONE));
}
fn draw(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(180, 100)).unwrap();
    terminal.draw(|frame| tui::render(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}
fn text_pages(app: &mut App) -> String {
    let mut rendered = String::new();
    for _ in 0..8 {
        rendered.push_str(&draw(app));
        key(app, KeyCode::Char('n'));
    }
    rendered
}
fn ready_tuple(readiness: &v2::ReadinessObservation) {
    assert_eq!(readiness.state, v2::ReadinessState::Ready);
    assert!(readiness.authenticated_instance);
    assert_eq!(readiness.initialized, Some(true));
    assert!(readiness.response_digest.is_some());
    assert_eq!(
        readiness.artifact_sha256.as_deref(),
        Some(readiness.expectation.artifact_sha256.as_str())
    );
    assert_eq!(
        readiness.version_literal.as_deref(),
        Some(readiness.expectation.version_literal.as_str())
    );
    assert_eq!(
        readiness.data_sha256.as_deref(),
        Some(readiness.expectation.data_sha256.as_str())
    );
    assert_eq!(
        readiness.configuration_digest.as_deref(),
        Some(readiness.expectation.configuration_digest.as_str())
    );
}

/// Accept rows decoded from the real MCP run_evidence record responses, or the
/// shipping saved-projection bridge. `detail` must contain the actual decoded JSON bytes.
pub fn assert_mcp_service_records(report: &NativeIntegrationReport, records: &[Value]) {
    let observations = report
        .integrations
        .iter()
        .flat_map(|scenario| &scenario.services)
        .flat_map(|service| &service.observations)
        .collect::<Vec<_>>();
    if report.outcome == RunOutcome::Passed {
        assert!(
            !observations.is_empty(),
            "passing service run needs actual observations"
        );
        assert!(
            observations
                .iter()
                .any(|observation| observation.readiness.is_some())
        );
    }
    for observation in observations {
        assert_eq!(observation.binding.source_profile, "native_integration_v1");
        assert!(!observation.binding.source_version.is_empty());
        if report.outcome == RunOutcome::Passed {
            assert!(observation.binding.complete);
        }
        match &observation.state {
            IntegrationProvenance::ManagerReported { manager, .. } => assert!(!manager.is_empty()),
            IntegrationProvenance::Unavailable { .. } if report.outcome != RunOutcome::Passed => (),
            other => panic!("fixture service fact has unexpected provenance: {other:?}"),
        }
        let row = records
            .iter()
            .find(|row| {
                row["record_kind"] == "service"
                    && row["detail"]["observation"]["binding"]["id"] == observation.binding.id
            })
            .expect("actual bound service observation must be public evidence");
        assert_eq!(
            row["detail"]["observation"],
            serde_json::to_value(observation).unwrap()
        );
        assert_eq!(
            row["detail"]["scenario_id"],
            observation.binding.scenario_id
        );
        assert_eq!(row["detail"]["slot"], observation.binding.slot.as_str());
        if let Some(readiness) = &observation.readiness {
            if readiness.state == v2::ReadinessState::Ready {
                ready_tuple(readiness);
            }
            if readiness.response_digest.is_some() {
                let application = records
                    .iter()
                    .filter(|row| row["record_kind"] == "readiness")
                    .filter_map(|row| row["detail"]["application_reports"].as_array())
                    .flatten()
                    .find(|fact| fact["observation_id"] == observation.binding.id)
                    .expect("application response tuple needs explicit public provenance");
                assert_eq!(application["provenance"], "application_reported");
                assert_eq!(
                    application["value"]["version_literal"],
                    serde_json::to_value(&readiness.version_literal).unwrap()
                );
                assert_eq!(
                    application["value"]["artifact_sha256"],
                    serde_json::to_value(&readiness.artifact_sha256).unwrap()
                );
                assert_eq!(
                    application["value"]["data_sha256"],
                    serde_json::to_value(&readiness.data_sha256).unwrap()
                );
                assert_eq!(
                    application["value"]["configuration_digest"],
                    serde_json::to_value(&readiness.configuration_digest).unwrap()
                );
                assert_eq!(
                    application["value"]["initialized"],
                    serde_json::to_value(readiness.initialized).unwrap()
                );
            }
        }
    }
    for scenario in &report.integrations {
        for check in &scenario.checks {
            if check.outcome == CheckOutcome::Passed
                && matches!(
                    check.predicate,
                    IntegrationPredicate::Service(ServicePredicate::ApplicationReady)
                        | IntegrationPredicate::ApplicationReady
                )
            {
                let matching = scenario
                    .services
                    .iter()
                    .flat_map(|service| &service.observations)
                    .filter(|observation| check.observation_ids.contains(&observation.binding.id))
                    .filter_map(|observation| observation.readiness.as_ref())
                    .collect::<Vec<_>>();
                assert!(
                    !matching.is_empty(),
                    "a label cannot establish application readiness"
                );
                for readiness in matching {
                    ready_tuple(readiness);
                }
            }
        }
    }
}

pub fn assert_service_views(report: NativeIntegrationReport, directory: &Path) {
    report.validate().unwrap();
    let StoredRun::NativeIntegration(saved) =
        theseus::native::storage::load_run(directory).unwrap()
    else {
        panic!("expected actual schema2 saved family")
    };
    assert_eq!(
        serde_json::to_value(&report).unwrap(),
        serde_json::to_value(saved).unwrap()
    );
    let public = theseus::cli::fixture_saved_native_projection(directory).unwrap();
    assert_eq!(public["saved"]["schema_version"], 2);
    assert_eq!(
        public["saved"]["outcome"],
        serde_json::to_value(report.outcome).unwrap()
    );
    assert_eq!(public["saved"]["integration"]["profile"], report.profile);
    let records = public["records"].as_array().unwrap();
    let header = records
        .iter()
        .find(|row| row["record_id"] == "run")
        .unwrap();
    assert_eq!(header["detail"]["recipe_digest"], report.recipe_digest);
    assert_eq!(header["detail"]["manifest_digest"], report.manifest_digest);
    assert_mcp_service_records(&report, records);
    let store = theseus::report::open_existing(directory).unwrap();
    let (_, NativeJournalVersion::IntegrationV2(journal)) =
        theseus::native::storage::load_native_versioned(&store).unwrap()
    else {
        panic!("expected schema2 paired journal")
    };
    let public_bytes = serde_json::to_string(&public).unwrap();
    for probe in journal.readiness_probes {
        assert!(
            !public_bytes.contains(&probe.challenge.nonce),
            "private challenge nonce escaped public evidence"
        );
    }
    let mut app = App::integration_offline(report.clone(), directory.to_owned());
    assert!(app.recipe.is_none() && app.report.is_none() && !app.active);
    let overview = draw(&app);
    assert!(overview.contains(&format!("Outcome: {}", report.outcome)));
    assert!(overview.contains(&report.profile));
    key(&mut app, KeyCode::Right);
    let service = text_pages(&mut app);
    assert!(service.contains("Services"));
    if report
        .integrations
        .iter()
        .flat_map(|scenario| &scenario.services)
        .any(|service| !service.observations.is_empty())
    {
        assert!(service.contains("observation"));
    }
    key(&mut app, KeyCode::Right);
    let readiness = text_pages(&mut app);
    assert!(readiness.contains("Readiness"));
    let first = report
        .integrations
        .iter()
        .flat_map(|scenario| &scenario.checks)
        .find(|check| {
            matches!(
                check.predicate,
                IntegrationPredicate::Service(ServicePredicate::ApplicationReady)
                    | IntegrationPredicate::ApplicationReady
            )
        });
    if let Some(first) = first {
        let response = report
            .integrations
            .iter()
            .flat_map(|scenario| &scenario.services)
            .flat_map(|service| &service.observations)
            .filter(|observation| first.observation_ids.contains(&observation.binding.id))
            .filter_map(|observation| observation.readiness.as_ref())
            .find(|readiness| readiness.response_digest.is_some());
        if let Some(response) = response {
            assert!(readiness.contains("application_reported"));
            assert!(readiness.contains("version_literal"));
            if let Some(version) = &response.version_literal {
                assert!(readiness.contains(version.trim()));
            }
        }
    }
    key(&mut app, KeyCode::Char('r'));
    assert!(
        !app.start_requested,
        "saved views cannot acquire execution authority"
    );
    assert_eq!(app.outcome(), report.outcome);
}
