use super::*;
#[test]
fn output_budget_survives_connection_epochs_and_covers_both_channels() {
    let mut recovery = Recovery::default();
    let lease = v2::LeaseId::from_bytes([1; 16]);
    let operation = v2::OperationId::from_bytes([2; 16]);
    for (channel, offset) in [
        (v2::Channel::Stdout, 0),
        (v2::Channel::Stderr, 0),
        (v2::Channel::Stdout, 4096),
        (v2::Channel::Stderr, 4096),
    ] {
        recovery
            .output(lease, operation, channel, offset, 4096)
            .unwrap();
    }
    // A new SessionGuard after reconnect must not replenish the live run's
    // shared raw-byte budget or accept an overlapping capture segment.
    assert_eq!(
        recovery.output(lease, operation, v2::Channel::Stdout, 8192, 1),
        Err(ClientError::Protocol)
    );
}
