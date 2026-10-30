use super::*;
use crate::tui::{App, Tab};
use std::iter::{empty, once};
type Records<'a> = Box<dyn Iterator<Item = Record<'a>> + 'a>;
/// The caller owns App/report storage. Rebuild after its revision/selection changes.
pub struct Catalog<'a> {
    app: &'a App,
}
impl<'a> Catalog<'a> {
    pub fn for_app(app: &'a App) -> Self {
        Self { app }
    }
    pub fn records(&self) -> impl Iterator<Item = SelectedRecord<'a>> + 'a {
        records(self.app)
            .enumerate()
            .map(|(key, record)| SelectedRecord { key, record })
    }
    pub fn count(&self) -> usize {
        self.records().count()
    }
    pub fn get(&self, key: usize) -> Option<SelectedRecord<'a>> {
        self.records().nth(key)
    }
}
fn records(app: &App) -> Records<'_> {
    let terminal = app
        .terminal_error
        .as_deref()
        .map(Record::TerminalError)
        .into_iter();
    let rest = if let Some(view) = &app.comparison {
        comparison(app, view)
    } else {
        lifecycle(app)
    };
    Box::new(terminal.chain(rest))
}
fn errors(errors: &[String]) -> impl Iterator<Item = Record<'_>> {
    errors.iter().map(|s| Record::Text(s))
}
fn lifecycle(app: &App) -> Records<'_> {
    let Some(stage) = app.stage() else {
        return Box::new(
            app.report.iter().flat_map(|r| errors(&r.errors)).chain(
                app.scenario()
                    .into_iter()
                    .flat_map(|s| errors(&s.errors).chain(once(Record::Teardown(&s.teardown)))),
            ),
        );
    };
    let proofs: Records<'_> =
        if matches!(app.tab, Tab::Stages | Tab::Commands) {
            Box::new(stage.restart.iter().map(Record::Restart).chain(
                stage.commands.iter().flat_map(|c| {
                    [
                        c.expected_failure.as_ref().map(Record::ExpectedFailure),
                        c.rejection.as_ref().map(|p| Record::DeclaredRefusal(c, p)),
                        c.interruption.as_ref().map(Record::Interruption),
                        c.interruption
                            .as_ref()
                            .filter(|p| {
                                c.outcome == CommandOutcome::Failed
                                    && c.exit_status.is_some()
                                    && !p.readiness_verified
                            })
                            .map(|_| Record::Barrier(c)),
                    ]
                    .into_iter()
                    .flatten()
                }),
            ))
        } else {
            Box::new(empty())
        };
    let rest: Records<'_> = match app.tab {
        Tab::Stages => Box::new(
            app.scenario()
                .and_then(verified_label)
                .map(Record::Text)
                .into_iter()
                .chain(once(Record::Stage(stage)))
                .chain(stage.checks.iter().map(Record::Check))
                .chain(errors(&stage.errors))
                .chain(stage.skip_reason.as_deref().map(Record::Skipped))
                .chain(stage.snapshots.iter().map(|s| Record::Observation(s, true))),
        ),
        Tab::Findings => Box::new(
            stage
                .checks
                .iter()
                .map(Record::Check)
                .chain(errors(&stage.errors))
                .chain(app.scenario().into_iter().flat_map(|s| {
                    s.findings
                        .iter()
                        .map(Record::Finding)
                        .chain(errors(&s.errors))
                        .chain(once(Record::Teardown(&s.teardown)))
                }))
                .chain(app.report.iter().flat_map(|r| errors(&r.errors)))
                .chain(
                    stage
                        .snapshots
                        .iter()
                        .filter(|s| s.completeness != ObservationStatus::Complete)
                        .map(Record::IncompleteObservation),
                ),
        ),
        Tab::Files => Box::new(
            stage
                .checks
                .iter()
                .filter(|c| c.comparison.is_some())
                .map(Record::Check)
                .chain(stage.stage_diff.iter().map(|d| Record::Change("Stage", d)))
                .chain(
                    stage
                        .baseline_diff
                        .iter()
                        .map(|d| Record::Change("Baseline", d)),
                )
                .chain(stage.snapshots.iter().flat_map(|s| {
                    once(Record::Observation(s, false))
                        .chain(s.entries.iter().map(|(p, e)| Record::Entry(p, e)))
                })),
        ),
        Tab::Commands => Box::new(stage.commands.iter().map(Record::Command)),
        Tab::Logs => Box::new(stage.commands.iter().flat_map(|c| {
            [
                c.stdout.as_ref().map(|r| Record::Log(c, "stdout", r)),
                c.stderr.as_ref().map(|r| Record::Log(c, "stderr", r)),
            ]
            .into_iter()
            .flatten()
        })),
    };
    let plugin = app.scenario().and_then(|s| s.plugin.as_ref());
    Box::new(
        plugin
            .into_iter()
            .filter(|_| matches!(app.tab, Tab::Stages | Tab::Findings))
            .map(Record::Plugin)
            .chain(
                app.report
                    .iter()
                    .filter(move |_| {
                        plugin.is_some() && matches!(app.tab, Tab::Stages | Tab::Findings)
                    })
                    .flat_map(|r| r.artifacts.iter().map(Record::Artifact)),
            )
            .chain(proofs)
            .chain(rest),
    )
}
fn verified_label(s: &ScenarioReport) -> Option<&'static str> {
    use crate::recipe::{DowngradeContract, RestorationPolicy};
    if s.scenario == ScenarioKind::Downgrade && s.outcome == RunOutcome::Passed {
        return match s.downgrade {
            Some(DowngradeContract::Compatible { .. }) => Some("Compatible downgrade passed"),
            Some(DowngradeContract::SafeRejection { .. }) => {
                Some("Declared rejection checks passed")
            }
            None => None,
        };
    }
    if s.scenario == ScenarioKind::Rollback && s.outcome == RunOutcome::Passed {
        return Some(match s.rollback.as_ref().map(|p| &p.recovery.restoration) {
            Some(RestorationPolicy::RestoreData { .. }) => {
                "Rollback verified: known-good binary, restored scopes and data readers passed"
            }
            _ => "Rollback verified: known-good binary and retained-data readers passed",
        });
    }
    if s.verified_interrupted_rollback() {
        return Some(
            match s
                .interrupted_rollback
                .as_ref()
                .map(|p| &p.recovery.restoration)
            {
                Some(RestorationPolicy::RestoreData { .. }) => {
                    "Interrupted rollback verified: same-ID restart, known-good binary, restored scopes and data readers passed"
                }
                _ => {
                    "Interrupted rollback verified: same-ID restart, known-good binary and retained-data readers passed"
                }
            },
        );
    }
    None
}
fn comparison<'a>(app: &'a App, view: &'a crate::tui::comparison::ComparisonView) -> Records<'a> {
    let Some(report) = &view.report else {
        return Box::new(empty());
    };
    let cell = report.cells.get(view.row);
    Box::new(
        cell.into_iter()
            .map(|c| Record::Cell(c, &view.manifest))
            .chain(cell.into_iter().flat_map(|c| {
                c.comparison
                    .diagnostics
                    .iter()
                    .map(|s| Record::Diagnostic(s))
            }))
            .chain(report.sources.iter().flat_map(|s| {
                once(Record::Source(s))
                    .chain(s.artifacts.iter().map(Record::Artifact))
                    .chain(
                        s.report_path
                            .as_deref()
                            .map(|p| Record::SourcePath(&app.root, p)),
                    )
                    .chain(s.error.as_deref().map(Record::Diagnostic))
            }))
            .chain(report.diagnostics.iter().map(|s| Record::Diagnostic(s)))
            .chain([Record::Cleanup(&app.root), Record::Json(&app.root)])
            .chain(cell.into_iter().flat_map(|c| {
                c.comparison
                    .differences
                    .iter()
                    .map(|d| Record::Change("Comparison", d))
            })),
    )
}
