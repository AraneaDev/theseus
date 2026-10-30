//! Synthetic browsing evidence only; this does not claim a real lifecycle ran.
use theseus::{model::*, snapshot::*};
pub fn metadata_report(files: usize, field_bytes: usize) -> RunReport {
    let mut report = RunReport::new("synthetic browsing");
    report.finished_at = Some("synthetic completion".into());
    report.selected_profiles = vec![ProfileName::User];
    report.selected_scenarios = vec![ScenarioKind::Clean];
    let mut scenario = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
    let mut stage = StageReport::new("configured");
    stage.outcome = StageOutcome::Passed;
    let mut snapshot = Snapshot {
        root: "/app".into(),
        completeness: ObservationStatus::Complete,
        entries: Default::default(),
    };
    let entry = FileEntry {
        kind: FileKind::Directory,
        mode: 0o755,
        uid: 10001,
        gid: 10001,
        size: 0,
        sha256: None,
        link_target: None,
    };
    snapshot.entries.insert("/app".into(), entry.clone());
    for index in 0..files {
        let mut file = entry.clone();
        file.kind = FileKind::File;
        if index + 1 == files && field_bytes > 0 {
            file.kind = FileKind::Symlink;
            file.link_target = Some(format!(
                "{}{}DETAIL_TAIL",
                "λ".repeat(field_bytes / 2),
                if field_bytes % 2 == 1 { "x" } else { "" }
            ));
        }
        snapshot
            .entries
            .insert(format!("/app/file-{index:05}"), file);
    }
    stage.snapshots.push(snapshot);
    scenario.stages.push(stage);
    scenario.teardown = TeardownStatus::Complete;
    scenario.outcome = RunOutcome::Passed;
    report.scenarios.push(scenario);
    report.outcome = RunOutcome::Passed;
    report
}
