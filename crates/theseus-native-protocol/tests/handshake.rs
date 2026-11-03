use theseus_native_protocol::{
    Decoder, Envelope, HandshakeGuard, Message, ProtocolError, Refusal, Role, SessionId, encode,
};
fn hello(role: Role) -> Envelope {
    Envelope {
        session: SessionId::from_bytes([3; 16]),
        sequence: 0,
        message: Message::Hello { role },
    }
}

#[test]
fn only_one_matching_peer_hello_establishes_a_handshake() {
    for role in [Role::Controller, Role::Helper] {
        let envelope = hello(role);
        let mut guard = HandshakeGuard::new(envelope.session.clone(), role);
        assert!(!guard.is_established());
        guard.accept(&envelope).unwrap();
        assert!(guard.is_established());
        assert_eq!(
            guard.accept(&envelope),
            Err(ProtocolError::UnexpectedMessage)
        );
        assert!(!guard.is_established());
        assert_eq!(
            guard.accept(&envelope),
            Err(ProtocolError::UnexpectedMessage)
        );
    }
}

#[test]
fn wrong_identity_sequence_role_and_refusal_are_terminal_with_defined_precedence() {
    let mut wrong_session = hello(Role::Helper);
    wrong_session.session = SessionId::from_bytes([9; 16]);
    wrong_session.sequence = 1;
    let mut wrong_sequence = hello(Role::Helper);
    wrong_sequence.sequence = 1;
    let mut refusal = hello(Role::Helper);
    refusal.message = Message::Refused {
        code: Refusal::UnsupportedVersion,
    };
    for (envelope, want) in [
        (wrong_session, ProtocolError::SessionMismatch),
        (wrong_sequence, ProtocolError::UnexpectedSequence),
        (hello(Role::Controller), ProtocolError::UnexpectedMessage),
        (refusal, ProtocolError::UnexpectedMessage),
    ] {
        let mut guard = HandshakeGuard::new(SessionId::from_bytes([3; 16]), Role::Helper);
        assert_eq!(guard.accept(&envelope), Err(want));
        assert!(!guard.is_established());
        assert_eq!(
            guard.accept(&hello(Role::Helper)),
            Err(ProtocolError::UnexpectedMessage)
        );
        assert!(!guard.is_established());
    }
}

#[test]
fn in_memory_controller_helper_handshakes_survive_every_wire_split() {
    for peer in [Role::Controller, Role::Helper] {
        let expected = hello(peer);
        let bytes = encode(&expected).unwrap();
        let stream = [bytes.as_slice(), bytes.as_slice()].concat();
        for split in 0..=stream.len() {
            let mut decoder = Decoder::new();
            let mut guard = HandshakeGuard::new(expected.session.clone(), peer);
            let mut frames = 0;
            for chunk in [&stream[..split], &stream[split..]] {
                let mut offset = 0;
                loop {
                    let progress = decoder.feed(&chunk[offset..]).unwrap();
                    offset += progress.consumed;
                    if let Some(envelope) = progress.frame {
                        assert_eq!(envelope, expected);
                        if frames == 0 {
                            guard.accept(&envelope).unwrap();
                            assert!(guard.is_established());
                        } else {
                            assert_eq!(
                                guard.accept(&envelope),
                                Err(ProtocolError::UnexpectedMessage)
                            );
                            assert!(!guard.is_established());
                        }
                        frames += 1;
                    }
                    if offset == chunk.len() {
                        break;
                    }
                    assert!(progress.consumed > 0);
                }
            }
            assert_eq!(frames, 2);
            assert!(!guard.is_established());
            decoder.finish().unwrap();
        }
    }
}
