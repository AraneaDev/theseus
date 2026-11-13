//! Pure outcome aggregation for stored native recovery.
use super::super::domain::{StoredJournal, StoredRun};
use super::disposition_outcome;
use crate::model::RunOutcome;

/// Aggregate outcome of the original integration journal; `Passed` for core journals.
pub(super) fn integration_journal_outcome(journal: &StoredJournal) -> RunOutcome {
    match journal {
        StoredJournal::NativeIntegration(j) => {
            let mut outcomes = vec![RunOutcome::Passed];
            outcomes.extend(
                j.operations
                    .iter()
                    .map(|o| disposition_outcome(&o.completion)),
            );
            outcomes.extend(
                j.releases
                    .iter()
                    .map(|r| disposition_outcome(&r.completion)),
            );
            outcomes.extend(
                j.integration_actions
                    .iter()
                    .map(|a| disposition_outcome(&a.completion)),
            );
            for resource in &j.integration_ownership {
                outcomes.push(disposition_outcome(&resource.disposition));
                if resource.revocation.is_some() {
                    outcomes.push(RunOutcome::Incomplete);
                }
            }
            outcomes.extend(
                j.readiness_probes
                    .iter()
                    .map(|p| disposition_outcome(&p.completion)),
            );
            outcomes.extend(
                j.broker_bindings
                    .iter()
                    .map(|b| disposition_outcome(&b.disposition)),
            );
            for attempt in &j.recovery_attempts {
                outcomes.extend(
                    [&attempt.prior_local, &attempt.local, &attempt.remote]
                        .map(disposition_outcome),
                );
            }
            for attempt in &j.integration_recoveries {
                outcomes.extend(attempt.prior_local.iter().map(disposition_outcome));
                outcomes.extend(
                    [&attempt.local, &attempt.broker, &attempt.provider].map(disposition_outcome),
                );
            }
            RunOutcome::aggregate(outcomes)
        }
        _ => RunOutcome::Passed,
    }
}

/// Fold scenario outcomes, retained failures and integration evidence into the report.
pub(super) fn aggregate_report(
    report: &mut StoredRun,
    original_integration_outcome: RunOutcome,
    has_failures: bool,
) {
    match report {
        StoredRun::Native(value) => {
            value.outcome = RunOutcome::aggregate(value.scenarios.iter().map(|s| s.outcome));
            if has_failures {
                value.failures.push(
                    "native recovery incomplete; original typed errors and owners retained".into(),
                );
            }
        }
        StoredRun::NativeIntegration(value) => {
            value.catalog.outcome =
                RunOutcome::aggregate(value.catalog.scenarios.iter().map(|s| s.outcome));
            // Recovery proves settlement only; it never upgrades missing required predicates.
            if has_failures {
                value.catalog.failures.push(
                    "native recovery incomplete; original typed errors and owners retained".into(),
                );
            }
            // Source failure and independent cleanup uncertainty are different
            // facts. Retain the original classification while adding only proved
            // new failure/uncertainty; missing predicates never become Passed.
            let mut outcomes = vec![
                value.outcome,
                value.catalog.outcome,
                original_integration_outcome,
            ];
            if has_failures {
                outcomes.push(RunOutcome::Incomplete);
            }
            // A journal row is not required for a retained core disposition
            // to remain unresolved (for example, Release never published).
            for scenario in &value.catalog.scenarios {
                outcomes.extend(
                    [
                        &scenario.dispositions.local,
                        &scenario.dispositions.remote,
                        &scenario.dispositions.release,
                        &scenario.dispositions.reset,
                    ]
                    .map(disposition_outcome),
                );
            }
            for integration in &value.integrations {
                outcomes.extend(integration.dispositions.all().map(disposition_outcome));
            }
            for selected in &value.selected_scenarios {
                let Some(core) = value
                    .catalog
                    .scenarios
                    .iter()
                    .find(|s| &s.scenario == selected)
                else {
                    outcomes.push(RunOutcome::Incomplete);
                    continue;
                };
                let Some(integration) = value
                    .integrations
                    .iter()
                    .find(|i| i.ownership_id == core.ownership_id)
                else {
                    outcomes.push(RunOutcome::Incomplete);
                    continue;
                };
                if let Some(service) = &value.requirements.service {
                    for required in service
                        .checks
                        .iter()
                        .filter(|c| c.scenarios.contains(selected))
                    {
                        outcomes.push(
                            match integration
                                .checks
                                .iter()
                                .find(|c| c.id == required.id)
                                .map(|c| c.outcome)
                            {
                                Some(crate::model::CheckOutcome::Passed) => RunOutcome::Passed,
                                Some(crate::model::CheckOutcome::Failed) => RunOutcome::Failed,
                                _ => RunOutcome::Incomplete,
                            },
                        );
                    }
                }
            }
            value.outcome = RunOutcome::aggregate(outcomes);
        }
        _ => unreachable!(),
    }
}
