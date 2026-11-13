use super::*;
use crate::{
    recovery::Recovery,
    transport::{Settlement, TransportOwner},
};
fn partial_failure(_: &NativeCommand, _: InputMode) -> Result<AcquiredChild, SpawnFailure> {
    Err(SpawnFailure {
        launch: theseus_host::process::LaunchPhase::NeverCreated,
        cleanup: theseus_host::process::HostCleanup::Pending(
            theseus_host::process::OwnedSpawnCleanup::from_endpoints(vec![], vec![]),
        ),
        error: std::io::Error::other("modeled retained spawn source"),
        cleanup_errors: vec![],
    })
}
#[test]
fn failed_reconnect_retains_partial_attempt_and_blocks_next_until_actual_join() {
    let mut client = Client {
        expected: crate::pump::tests::expected(),
        settlement: Settlement::new(TransportOwner::empty()),
        recovery: Recovery::default(),
        query_only: false,
        next_id: Some(73),
        error: None,
        lost: false,
    };
    client.request_stop();
    assert!(client.is_settled());
    let mut command = NativeCommand::new(if cfg!(windows) {
        "C:\\trusted-modeled-fixture.exe"
    } else {
        "/trusted-modeled-fixture"
    });
    command.environment = theseus_host::process::ChildEnvironment::Replace(vec![]);
    assert_eq!(
        client.reconnect_with(command.clone(), partial_failure),
        Err(ClientError::Spawn)
    );
    assert!(!client.is_settled());
    assert_eq!(
        client.reconnect_with(command, partial_failure),
        Err(ClientError::NotSettled)
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !client.poll_settle().unwrap() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(
        client
            .failure()
            .unwrap()
            .source()
            .unwrap()
            .is::<SpawnFailure>()
    );
    assert_eq!(client.next_id, Some(73));
}
#[test]
fn readiness_is_required_before_mutation_admission() {
    let mut owner = TransportOwner::empty();
    owner.pump = Some(crate::pump::Pump::new(v2::SessionId::from_bytes([7; 16])).unwrap());
    let mut client = Client {
        expected: crate::pump::tests::expected(),
        settlement: Settlement::new(owner),
        recovery: Recovery::default(),
        query_only: false,
        next_id: Some(0),
        error: None,
        lost: false,
    };
    assert_eq!(
        client.submit(Action::Run {
            lease: v2::LeaseId::from_bytes([1; 16]),
            operation: v2::OperationId::from_bytes([2; 16]),
            program: v2::ProgramId::new("probe").unwrap()
        }),
        Err(ClientError::NotReady)
    );
    let frames = &client
        .settlement
        .owner
        .as_ref()
        .unwrap()
        .pump
        .as_ref()
        .unwrap()
        .frames;
    assert_eq!(frames.len(), 1);
    let hello = v2::Decoder::new()
        .feed(&frames[0].bytes)
        .unwrap()
        .frame
        .unwrap();
    assert!(matches!(hello.message(), v2::Message::Hello { .. }));
    assert_eq!(client.next_id, Some(0));
}
