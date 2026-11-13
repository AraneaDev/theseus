use super::*;
use crate::transport::{Settlement, TransportOwner};
fn offline(next: Option<u64>) -> Client {
    let mut owner = TransportOwner::empty();
    owner.pump = Some(crate::pump::tests::ready());
    Client {
        expected: crate::pump::tests::expected(),
        settlement: Settlement::new(owner),
        recovery: crate::recovery::Recovery::default(),
        query_only: false,
        next_id: next,
        error: None,
        lost: false,
    }
}
#[test]
fn maximum_public_id_is_issued_once_then_refused_before_queueing() {
    let mut client = offline(Some(u64::MAX));
    let lease = v2::LeaseId::from_bytes([4; 16]);
    assert_eq!(
        client.submit(Action::QueryLease(lease)).unwrap(),
        RequestId(u64::MAX)
    );
    // Release the ordinary slot by installing a new wire epoch. This cannot
    // replenish the independent public counter.
    client.settlement.owner.as_mut().unwrap().pump = Some(crate::pump::tests::ready());
    assert_eq!(
        client.submit(Action::QueryLease(lease)),
        Err(ClientError::Exhausted)
    );
    assert!(
        client
            .settlement
            .owner
            .as_ref()
            .unwrap()
            .pump
            .as_ref()
            .unwrap()
            .frames
            .is_empty()
    );
}
#[test]
fn failed_reconnect_does_not_reset_public_namespace() {
    let mut client = offline(Some(17));
    let lease = v2::LeaseId::from_bytes([4; 16]);
    let old = client.submit(Action::QueryLease(lease)).unwrap();
    client.request_stop();
    assert!(client.poll_settle().unwrap());
    let mut command = NativeCommand::new("relative-program");
    command.environment = theseus_host::process::ChildEnvironment::Replace(vec![]);
    assert_eq!(client.reconnect(command), Err(ClientError::InvalidCommand));
    client.settlement = Settlement::new({
        let mut owner = TransportOwner::empty();
        owner.pump = Some(crate::pump::tests::ready());
        owner
    });
    let new = client.submit(Action::QueryLease(lease)).unwrap();
    assert_eq!(old, RequestId(17));
    assert_eq!(new, RequestId(18));
    let bytes = &client
        .settlement
        .owner
        .as_ref()
        .unwrap()
        .pump
        .as_ref()
        .unwrap()
        .frames[0]
        .bytes;
    let frame = v2::Decoder::new().feed(bytes).unwrap().frame.unwrap();
    assert_eq!(frame.sequence(), 2);
}
