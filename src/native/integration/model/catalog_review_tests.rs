use super::*;
#[test]
fn review_rejects_malformed_catalog_evidence() {
    let mut report = crate::native::integration::view::tests::report();
    report.catalog.scenarios[0].stages.push(NativeStage {
        name: "malformed-catalog".into(),
        step: None,
        checkpoint: None,
        outcome: crate::model::StageOutcome::Passed,
        commands: vec![NativeCommandEvidence {
            command: CatalogCommand {
                id: "probe".into(),
                program: CatalogProgram::Exercise,
                expected_status: NativeStatus::PosixExit { code: 0 },
                stdout_literal: None,
            },
            operation_id: "invalid".into(),
            request_digest: "not-a-digest".into(),
            status: Some(NativeStatus::PosixExit { code: 0 }),
            stdout: String::new(),
            stdout_complete: true,
            stdout_utf8: true,
            stderr_complete: true,
            stdout_log: None,
            stderr_log: None,
            failure: None,
        }],
        checks: vec![],
        native_files: vec![],
        failures: vec![],
    });
    assert!(
        report.validate().is_err(),
        "malformed catalog command must refuse schema2 evidence"
    );
    report.catalog.scenarios[0].stages[0].commands.clear();
    report.catalog.scenarios[0].stages[0]
        .native_files
        .push(NativeFileObservation {
            request_id: "inspect".into(),
            root_identity: "root".into(),
            leaf: "unexpected".into(),
            state: NativeFileState::Present {
                length: 1,
                sha256: "bad".into(),
            },
        });
    assert!(
        report.validate().is_err(),
        "malformed catalog file evidence must refuse"
    );
    report.catalog.scenarios[0].stages[0].native_files.clear();
    report.catalog.scenarios[0].outcome = RunOutcome::Passed;
    assert!(
        report.validate().is_err(),
        "passed catalog with unresolved core dispositions must refuse"
    );
}
