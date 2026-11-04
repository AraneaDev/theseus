#![cfg(all(feature = "native-integration-test-support", target_os = "linux"))]
//! Real supplied private-manager product acceptance. An absent fixture is failure.
#[path = "support/native_service_setup.rs"]
mod setup;
#[path = "support/native_service_views.rs"]
mod views;
use setup::{Fault, IntegrationFixture};
use std::{
    path::PathBuf,
    process::{Command, Output},
};
use theseus::{
    engine::domain::StoredRun,
    model::RunOutcome,
    native::{integration::model::*, storage},
};
fn run(fixture: &IntegrationFixture) -> (Output, NativeIntegrationReport, PathBuf) {
    fixture.authority.validate().unwrap();
    fixture.recipe.recipe.validate().unwrap();
    assert!(fixture.ready_root.path().is_dir());
    let controller = &fixture.controller_path;
    let output = Command::new(controller)
        .args(["--fixture-graph"])
        .arg(&fixture.graph_path)
        .args(["--no-animation", "run", "--headless", "--recipe"])
        .arg(&fixture.recipe_path)
        .args(["--native-target"])
        .arg(fixture.root.path().join("launch.json"))
        .args(["--scenario", "clean"])
        .output()
        .unwrap();
    let dirs = std::fs::read_dir(&fixture.output)
        .unwrap_or_else(|error| {
            panic!(
                "missing run: {error}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(dirs.len(), 1, "one original run");
    let StoredRun::NativeIntegration(report) = storage::load_run(&dirs[0]).unwrap() else {
        panic!("schema2 report required")
    };
    if !output.status.success() {
        eprintln!(
            "saved failed service report: {}",
            serde_json::to_string_pretty(&report).unwrap()
        );
    }
    (output, report, dirs[0].clone())
}
#[test]
fn systemd_user_clean_installer_managed_service_uses_shared_product_path() {
    let fixture =
        IntegrationFixture::supplied_manager(ManagerKind::SystemdUser, Fault::None).unwrap();
    let (output, report, _) = run(&fixture);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(report.outcome, RunOutcome::Passed);
    assert!(
        report
            .integrations
            .iter()
            .all(|i| i.dispositions.complete())
    );
    assert!(
        report.integrations[0]
            .checks
            .iter()
            .any(|c| c.predicate
                == IntegrationPredicate::Service(ServicePredicate::ApplicationReady))
    );
}
#[test]
fn systemd_installer_missing_service_is_failed_without_harness_repair() {
    let fixture =
        IntegrationFixture::supplied_manager(ManagerKind::SystemdUser, Fault::MissingService)
            .unwrap();
    let (output, report, _) = run(&fixture);
    assert!(!output.status.success());
    assert_ne!(report.outcome, RunOutcome::Passed);
    assert!(report.integrations[0].checks.iter().any(|c| c.predicate
        == IntegrationPredicate::Service(ServicePredicate::Registration)
        && c.outcome == theseus::model::CheckOutcome::Failed));
    assert!(
        report.integrations[0].services[0].actions.is_empty(),
        "missing registration cannot grant helper start"
    );
}
#[test]
fn systemd_uninstaller_leaves_service_is_failed_before_and_after_provider_disposal() {
    let fixture =
        IntegrationFixture::supplied_manager(ManagerKind::SystemdUser, Fault::LeakedService)
            .unwrap();
    let (output, report, _) = run(&fixture);
    assert!(!output.status.success());
    assert_ne!(report.outcome, RunOutcome::Passed);
    assert!(
        report.integrations[0]
            .checks
            .iter()
            .any(|c| c.phase == IntegrationPhase::Uninstalled
                && c.predicate
                    == IntegrationPredicate::Service(ServicePredicate::RegistrationAbsent)
                && c.outcome == theseus::model::CheckOutcome::Failed)
    );
    let claims = &report.catalog.provider_claims;
    assert!(
        claims.iter().any(|r| r
            .receipt
            .starts_with("model-provider-reservation-released-")),
        "provider disposition is separate from failed SUT removal"
    );
}

#[test]
fn systemd_model_clean_exercises_real_helper_installer_and_application() {
    let receipt = PathBuf::from(
        std::env::var_os("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT")
            .expect("explicit local model receipt required; cannot replace supplied-manager tests"),
    );
    let fixture = IntegrationFixture::model_manager(&receipt, Fault::None).unwrap();
    let (output, report, directory) = run(&fixture);
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(report.outcome, RunOutcome::Passed);
    views::assert_service_views(report.clone(), &directory);
    assert!(
        report.integrations[0]
            .checks
            .iter()
            .all(|c| c.outcome == theseus::model::CheckOutcome::Passed)
    );
}

#[test]
fn systemd_model_missing_and_leaked_registration_never_passes() {
    let receipt = PathBuf::from(
        std::env::var_os("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT")
            .expect("explicit local model receipt required"),
    );
    for fault in [Fault::MissingService, Fault::LeakedService] {
        let fixture = IntegrationFixture::model_manager(&receipt, fault).unwrap();
        let (output, report, directory) = run(&fixture);
        assert!(!output.status.success());
        assert_ne!(report.outcome, RunOutcome::Passed);
        assert!(
            report.integrations[0]
                .checks
                .iter()
                .any(|c| c.outcome == theseus::model::CheckOutcome::Failed)
        );
        views::assert_service_views(report, &directory);
    }
}
#[test]
fn systemd_model_running_wrong_version_data_peer_or_nonce_never_passes_readiness() {
    let receipt = PathBuf::from(
        std::env::var_os("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT")
            .expect("explicit local model receipt required"),
    );
    for fault in [
        Fault::WrongVersion,
        Fault::WrongData,
        Fault::WrongPeer,
        Fault::WrongNonce,
    ] {
        let fixture = IntegrationFixture::model_manager(&receipt, fault).unwrap();
        let (output, report, directory) = run(&fixture);
        assert!(!output.status.success());
        assert_ne!(report.outcome, RunOutcome::Passed);
        let observations = &report.integrations[0].services[0].observations;
        assert!(
            observations.iter().any(|o| matches!(
                &o.state,
                IntegrationProvenance::ManagerReported {
                    value: ServiceState::Present { running: true, .. },
                    ..
                }
            )),
            "manager really reported Running"
        );
        assert!(
            observations
                .iter()
                .filter_map(|o| o.readiness.as_ref())
                .any(|r| r.state != theseus_native_protocol::v2::ReadinessState::Ready),
            "independent application readiness must fail"
        );
        views::assert_service_views(report, &directory);
    }
}

fn assert_preexisting_is_untouched(fixture: IntegrationFixture) {
    let (unit, original) = fixture.place_preexisting_definition().unwrap();
    let (output, report, directory) = run(&fixture);
    assert!(!output.status.success());
    assert_ne!(report.outcome, RunOutcome::Passed);
    assert_eq!(
        std::fs::read(unit).unwrap(),
        original,
        "foreign preexisting definition remains untouched"
    );
    let journal = theseus::report::open_existing(&directory)
        .unwrap()
        .load_selected_journal()
        .unwrap();
    let theseus::engine::domain::StoredJournal::NativeIntegration(journal) = journal else {
        panic!("schema2 journal expected")
    };
    assert!(
        journal.operations.is_empty() && journal.integration_actions.is_empty(),
        "no SUT install or manager control can adopt an existing registration"
    );
}
#[test]
fn systemd_preexisting_or_wrong_owned_unit_is_untouched() {
    assert_preexisting_is_untouched(
        IntegrationFixture::supplied_manager(ManagerKind::SystemdUser, Fault::None).unwrap(),
    );
}
#[test]
fn systemd_model_preexisting_or_wrong_owned_unit_is_untouched() {
    let receipt = PathBuf::from(
        std::env::var_os("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT")
            .expect("explicit local model receipt required"),
    );
    assert_preexisting_is_untouched(
        IntegrationFixture::model_manager(&receipt, Fault::None).unwrap(),
    );
}
