use super::*;
#[test]
fn terminal_counters_on_new_connection_cannot_erase_pre_loss_bytes() {
    let mut recovery = Recovery::default();
    let lease = v2::LeaseId::from_bytes([1; 16]);
    let operation = v2::OperationId::from_bytes([2; 16]);
    recovery
        .output(lease, operation, v2::Channel::Stdout, 100, 12)
        .unwrap();
    let reply = v2::Reply::Operation {
        lease,
        operation,
        state: v2::OperationView::Finished {
            outcome: v2::Outcome::RunCompleted {
                reason: v2::RunReason::Natural,
                status: v2::NativeStatus::PosixExit { code: 0 },
                cleanup: v2::Cleanup::PosixLeaderReapedGroupTerminationRequested,
                stdout_bytes: 111,
                stderr_bytes: 0,
                truncated: false,
            },
        },
    };
    assert_eq!(recovery.observe(&reply), Err(ClientError::Protocol));
}
