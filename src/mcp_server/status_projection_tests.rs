use super::*;
use crate::model::*;
fn snapshot() -> ReportSnapshot {
    ReportSnapshot {
        revision: "a".repeat(64),
        report: RunReport::new("saved"),
        sampled_hint: 7,
    }
}
#[test]
fn saved_view_uses_loaded_outcome_and_existing_counts_without_recomputation() {
    let mut snap = snapshot();
    snap.report.schema_version = 1;
    snap.report.outcome = RunOutcome::Passed;
    snap.report.finished_at = Some("done".into());
    let view = saved_view(&snap).unwrap();
    assert_eq!(view.outcome, RunOutcome::Passed);
    assert_eq!(view.schema_version, 1);
    assert_eq!(view.revision.as_str(), snap.revision);
    assert_eq!(view.counts.commands.get(), 0);
    assert_eq!(view.finished_at.unwrap().text, "done");
    let projected = project_snapshot(&snap).unwrap();
    assert_eq!(projected.saved.outcome, RunOutcome::Passed);
    assert_eq!(projected.guest, GuestCleanup::Unknown);
    assert_eq!(projected.sampled_hint, 7);
    snap.sampled_hint = 9001;
    let later = project_snapshot(&snap).unwrap();
    assert_eq!(later.sampled_hint, 9001);
    assert_eq!(later.saved.revision, projected.saved.revision);
}
#[test]
fn running_guest_is_in_progress_and_terminal_cleanup_needs_teardown_proof() {
    let mut snap = snapshot();
    assert_eq!(guest_cleanup(None, false, false), GuestCleanup::NotStarted);
    assert_eq!(guest_cleanup(None, false, true), GuestCleanup::Unknown);
    assert_eq!(
        guest_cleanup(Some(&snap), false, true),
        GuestCleanup::Unknown
    );
    let mut s = ScenarioReport::new("s", ProfileName::User, ScenarioKind::Clean);
    s.teardown = TeardownStatus::Complete;
    snap.report.scenarios.push(s);
    assert_eq!(
        guest_cleanup(Some(&snap), true, true),
        GuestCleanup::InProgress
    );
    assert_eq!(
        guest_cleanup(Some(&snap), false, true),
        GuestCleanup::Complete
    );
    snap.report.scenarios[0].teardown = TeardownStatus::Failed {
        reason: "failed".into(),
    };
    assert_eq!(
        guest_cleanup(Some(&snap), false, true),
        GuestCleanup::Failed
    );
    assert_eq!(
        guest_cleanup(Some(&snap), true, true),
        GuestCleanup::InProgress
    );
    snap.report.scenarios[0].teardown = TeardownStatus::Pending;
    assert_eq!(
        guest_cleanup(Some(&snap), false, true),
        GuestCleanup::Unknown
    );
}
#[test]
fn cleanup_recovery_keeps_historical_launch_and_error_facts_separate() {
    assert!(
        !cleanup_view(
            NativeCleanup::Complete,
            None,
            GuestCleanup::Unknown,
            false,
            false,
            false
        )
        .recovery_required
    );
    assert!(
        cleanup_view(
            NativeCleanup::Unknown,
            None,
            GuestCleanup::Complete,
            true,
            false,
            false
        )
        .recovery_required
    );
    assert!(
        cleanup_view(
            NativeCleanup::Complete,
            None,
            GuestCleanup::Failed,
            false,
            false,
            false
        )
        .recovery_required,
        "saved failed teardown is itself an error even when engine returned Ok"
    );
    assert!(
        cleanup_view(
            NativeCleanup::Complete,
            None,
            GuestCleanup::Failed,
            true,
            false,
            false
        )
        .recovery_required
    );
    let v = cleanup_view(
        NativeCleanup::Complete,
        Some(Launch::MayRun),
        GuestCleanup::Complete,
        false,
        true,
        true,
    );
    assert!(v.recovery_required);
    assert_eq!(v.launch, Some(Launch::MayRun));
}

#[test]
fn count_projection_preserves_each_counter_and_rejects_nonexact_summary_numbers() {
    let source = ResultCounts {
        passed_checks: 1,
        failed_checks: 2,
        incomplete_checks: 3,
        skipped_checks: 4,
        review_findings: 5,
        commands: 6,
    };
    let projected = counts(&source).unwrap();
    assert_eq!(
        serde_json::to_value(projected).unwrap(),
        serde_json::json!({"passed_checks":1,"failed_checks":2,"incomplete_checks":3,"skipped_checks":4,"review_findings":5,"commands":6})
    );
    if let Ok(overflow) = usize::try_from(UINT_MAX + 1) {
        for index in 0..6 {
            let mut source = ResultCounts::default();
            match index {
                0 => source.passed_checks = overflow,
                1 => source.failed_checks = overflow,
                2 => source.incomplete_checks = overflow,
                3 => source.skipped_checks = overflow,
                4 => source.review_findings = overflow,
                _ => source.commands = overflow,
            };
            assert_eq!(
                counts(&source).unwrap_err().code,
                ErrorCode::ResponseTooLarge
            );
        }
    }
}
