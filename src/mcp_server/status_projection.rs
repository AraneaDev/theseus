//! Pure immutable saved/cleanup projections, with no native owner access.
use super::{
    evidence::{ReportSnapshot, error},
    types::*,
};
use crate::model::{ResultCounts, TeardownStatus};
#[derive(Debug, Clone)]
pub(crate) struct SavedProjection {
    pub saved: SavedView,
    pub guest: GuestCleanup,
    pub sampled_hint: u64,
}
/// Query workers publish this immutable pair before releasing their sole full
/// report snapshot. The registry separately overrides guest while running.
pub(crate) fn project_snapshot(snapshot: &ReportSnapshot) -> Result<SavedProjection, QueryError> {
    Ok(SavedProjection {
        saved: saved_view(snapshot)?,
        guest: guest_cleanup(Some(snapshot), false, true),
        sampled_hint: snapshot.sampled_hint,
    })
}
fn count(value: usize) -> Result<UInt, QueryError> {
    u64::try_from(value)
        .ok()
        .and_then(|n| UInt::new(n).ok())
        .ok_or_else(|| error(ErrorCode::ResponseTooLarge))
}
pub(crate) fn counts(source: &ResultCounts) -> Result<Counts, QueryError> {
    Ok(Counts {
        passed_checks: count(source.passed_checks)?,
        failed_checks: count(source.failed_checks)?,
        incomplete_checks: count(source.incomplete_checks)?,
        skipped_checks: count(source.skipped_checks)?,
        review_findings: count(source.review_findings)?,
        commands: count(source.commands)?,
    })
}
pub(crate) fn saved_view(snapshot: &ReportSnapshot) -> Result<SavedView, QueryError> {
    let report = &snapshot.report;
    let schema_version = u8::try_from(report.schema_version)
        .ok()
        .filter(|v| (1..=8).contains(v))
        .ok_or_else(|| error(ErrorCode::EvidenceInvalid))?;
    Ok(SavedView {
        native_dispositions: None,
        revision: Revision::parse(&snapshot.revision)
            .map_err(|_| error(ErrorCode::EvidenceInvalid))?,
        schema_version,
        outcome: report.outcome,
        counts: counts(&report.counts())?,
        finished_at: report
            .finished_at
            .as_ref()
            .map(|s| TextPreview::new(s, false)),
    })
}
pub(crate) fn guest_cleanup(
    snapshot: Option<&ReportSnapshot>,
    execution_running: bool,
    execution_started: bool,
) -> GuestCleanup {
    if execution_running {
        return GuestCleanup::InProgress;
    }
    let Some(snapshot) = snapshot else {
        return if execution_started {
            GuestCleanup::Unknown
        } else {
            GuestCleanup::NotStarted
        };
    };
    let scenarios = &snapshot.report.scenarios;
    if scenarios
        .iter()
        .any(|s| matches!(s.teardown, TeardownStatus::Failed { .. }))
    {
        GuestCleanup::Failed
    } else if !scenarios.is_empty()
        && scenarios
            .iter()
            .all(|s| matches!(s.teardown, TeardownStatus::Complete))
    {
        GuestCleanup::Complete
    } else {
        GuestCleanup::Unknown
    }
}
/// `after_error` means an actual admission/execution/worker failure, supplied
/// from published registry facts; a query refresh failure does not set it.
/// A saved failed teardown is itself an error even when execution returned Ok.
/// Native cleanup and strongest historical launch are already-published facts,
/// never inferred here from guest evidence, report outcome, or absence of proof.
pub(crate) fn cleanup_view(
    native: NativeCleanup,
    launch: Option<Launch>,
    guest: GuestCleanup,
    after_error: bool,
    worker_error: bool,
    journal_known: bool,
) -> CleanupView {
    let after_error = after_error || matches!(guest, GuestCleanup::Failed);
    let recovery_required = after_error
        && (matches!(native, NativeCleanup::Pending | NativeCleanup::Unknown)
            || matches!(guest, GuestCleanup::Failed | GuestCleanup::Unknown))
        || worker_error && journal_known;
    CleanupView {
        native,
        guest,
        launch,
        recovery_required,
    }
}
#[cfg(test)]
#[path = "status_projection_tests.rs"]
mod tests;

pub(super) fn project_native_snapshot(
    snapshot: &super::native_evidence::Snapshot,
) -> Result<SavedProjection, QueryError> {
    use crate::{model::CheckOutcome, native::model::Disposition};
    let report = &snapshot.report;
    let mut total = ResultCounts::default();
    for scenario in &report.scenarios {
        for stage in &scenario.stages {
            total.commands += stage.commands.len();
            for check in &stage.checks {
                match check.outcome {
                    CheckOutcome::Passed => total.passed_checks += 1,
                    CheckOutcome::Failed => total.failed_checks += 1,
                    CheckOutcome::Incomplete => total.incomplete_checks += 1,
                    CheckOutcome::Skipped => total.skipped_checks += 1,
                }
            }
        }
    }
    let guest = if report.scenarios.iter().any(|s| {
        matches!(s.dispositions.remote, Disposition::Failed { .. })
            || matches!(s.dispositions.release, Disposition::Failed { .. })
            || matches!(s.dispositions.reset, Disposition::Failed { .. })
    }) {
        GuestCleanup::Failed
    } else if !report.scenarios.is_empty()
        && report.scenarios.iter().all(|s| {
            matches!(s.dispositions.remote, Disposition::Complete { .. })
                && matches!(s.dispositions.release, Disposition::Complete { .. })
                && matches!(s.dispositions.reset, Disposition::Complete { .. })
        })
    {
        GuestCleanup::Complete
    } else {
        GuestCleanup::Unknown
    };
    Ok(SavedProjection {
        saved: SavedView {
            native_dispositions: Some(
                report
                    .scenarios
                    .iter()
                    .map(|s| {
                        let view = |d: &Disposition| match d {
                            Disposition::NotStarted => NativeDispositionView::NotStarted,
                            Disposition::Complete { receipt } => NativeDispositionView::Complete {
                                receipt: TextPreview::new(receipt, false),
                            },
                            Disposition::Unknown { reason } => NativeDispositionView::Unknown {
                                reason: TextPreview::new(reason, false),
                            },
                            Disposition::Failed { reason } => NativeDispositionView::Failed {
                                reason: TextPreview::new(reason, false),
                            },
                        };
                        NativeScenarioDispositions {
                            scenario_id: TextPreview::new(&s.id, false),
                            local: view(&s.dispositions.local),
                            remote: view(&s.dispositions.remote),
                            release: view(&s.dispositions.release),
                            reset: view(&s.dispositions.reset),
                        }
                    })
                    .collect(),
            ),
            revision: Revision::parse(&snapshot.revision)
                .map_err(|_| error(ErrorCode::EvidenceInvalid))?,
            schema_version: 1,
            outcome: report.outcome,
            counts: counts(&total)?,
            finished_at: None,
        },
        guest,
        sampled_hint: snapshot.sampled_hint,
    })
}
