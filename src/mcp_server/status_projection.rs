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
        integration: None,
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
    use crate::native::{integration::model::NativeReportVersion, model::Disposition};
    let report = &snapshot.report;
    let scenarios = report.consumer_scenarios();
    let total = report.consumer_counts();
    let integration = match report {
        NativeReportVersion::IntegrationV2(r) => Some(crate::native::integration::view::summary(r)),
        _ => None,
    };
    let catalog_guest = if scenarios.iter().any(|s| {
        matches!(s.dispositions.remote, Disposition::Failed { .. })
            || matches!(s.dispositions.release, Disposition::Failed { .. })
            || matches!(s.dispositions.reset, Disposition::Failed { .. })
    }) {
        GuestCleanup::Failed
    } else if !scenarios.is_empty()
        && scenarios.iter().all(|s| {
            matches!(s.dispositions.remote, Disposition::Complete { .. })
                && matches!(s.dispositions.release, Disposition::Complete { .. })
                && matches!(s.dispositions.reset, Disposition::Complete { .. })
        })
    {
        GuestCleanup::Complete
    } else {
        GuestCleanup::Unknown
    };
    let guest = if let Some(summary) = &integration {
        let flags = [
            &summary.helper_worker,
            &summary.registration,
            &summary.manager_action,
            &summary.broker,
            &summary.primary,
            &summary.descendant_scope,
        ];
        if matches!(catalog_guest, GuestCleanup::Failed) || flags.iter().any(|f| f.failed != 0) {
            GuestCleanup::Failed
        } else if matches!(catalog_guest, GuestCleanup::Complete)
            && flags
                .iter()
                .all(|f| f.resources != 0 && f.complete == f.resources)
        {
            GuestCleanup::Complete
        } else {
            GuestCleanup::Unknown
        }
    } else {
        catalog_guest
    };
    Ok(SavedProjection {
        saved: SavedView {
            integration,
            native_dispositions: Some(
                scenarios
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
            schema_version: if matches!(report, NativeReportVersion::IntegrationV2(_)) {
                2
            } else {
                1
            },
            outcome: report.consumer_outcome(),
            counts: counts(&total)?,
            finished_at: None,
        },
        guest,
        sampled_hint: snapshot.sampled_hint,
    })
}
