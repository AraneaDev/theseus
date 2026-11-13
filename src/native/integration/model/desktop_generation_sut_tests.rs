use super::*;
use theseus_native_protocol::v2;
#[test]
fn generation_requires_original_successful_sut_lease_program_digest_and_measured_status() {
    let catalog = CatalogCommand {
        id: "install_target".into(),
        program: CatalogProgram::InstallTarget,
        expected_status: NativeStatus::PosixExit { code: 0 },
        stdout_literal: None,
    };
    let mut operation = NativeOperation {
        ownership_id: "owner".into(),
        lease: "01".repeat(16),
        operation_id: "02".repeat(16),
        request_digest: String::new(),
        command: catalog.clone(),
        completion: Disposition::Complete {
            receipt: "original accepted Run completed".into(),
        },
    };
    operation.request_digest = v2::request_digest(&operation.request().unwrap())
        .unwrap()
        .to_hex();
    let witness = v2::broker::DesktopSutRunWitness {
        lease: operation.lease.clone(),
        operation: operation.operation_id.clone(),
        request_digest: operation.request_digest.clone(),
        program: catalog.program.program_id(),
    };
    let measured = NativeCommandEvidence {
        command: catalog,
        operation_id: operation.operation_id.clone(),
        request_digest: operation.request_digest.clone(),
        status: Some(NativeStatus::PosixExit { code: 0 }),
        stdout: String::new(),
        stdout_complete: true,
        stdout_utf8: true,
        stderr_complete: true,
        stdout_log: None,
        stderr_log: None,
        failure: None,
    };
    validate_desktop_sut_generation("owner", &operation.lease, &witness, &operation, &measured)
        .unwrap();
    let mut foreign = witness.clone();
    foreign.operation = "03".repeat(16);
    assert!(
        validate_desktop_sut_generation("owner", &operation.lease, &foreign, &operation, &measured)
            .is_err()
    );
    let mut lost = operation.clone();
    lost.completion = Disposition::Unknown {
        reason: "original completion lost".into(),
    };
    assert!(
        validate_desktop_sut_generation("owner", &operation.lease, &witness, &lost, &measured)
            .is_err()
    );
    let mut failed = measured.clone();
    failed.status = Some(NativeStatus::PosixExit { code: 1 });
    assert!(
        validate_desktop_sut_generation("owner", &operation.lease, &witness, &operation, &failed)
            .is_err()
    );
    assert!(
        validate_desktop_sut_generation(
            "other-owner",
            &operation.lease,
            &witness,
            &operation,
            &measured
        )
        .is_err()
    );
    assert!(
        validate_desktop_sut_generation("owner", &"04".repeat(16), &witness, &operation, &measured)
            .is_err()
    );
    let mut changed = measured;
    changed.command.program = CatalogProgram::Configure;
    assert!(
        validate_desktop_sut_generation("owner", &operation.lease, &witness, &operation, &changed)
            .is_err()
    );
}
