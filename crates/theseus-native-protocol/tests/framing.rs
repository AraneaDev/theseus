use theseus_native_protocol::{Decoder, Envelope, Message, ProtocolError, Role, SessionId, encode};

fn hello() -> Envelope {
    Envelope {
        session: SessionId::from_bytes([7; 16]),
        sequence: 0,
        message: Message::Hello {
            role: Role::Controller,
        },
    }
}
fn framed(body: &[u8]) -> Vec<u8> {
    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(body);
    frame
}

#[test]
fn valid_frame_is_invariant_under_every_split_and_byte_at_a_time() {
    let expected = hello();
    let bytes = encode(&expected).unwrap();
    for split in 0..=bytes.len() {
        let mut d = Decoder::new();
        let a = d.feed(&bytes[..split]).unwrap();
        assert_eq!(a.consumed, split);
        let b = d.feed(&bytes[split..]).unwrap();
        assert_eq!(b.consumed, bytes.len() - split);
        assert_eq!(a.frame.or(b.frame), Some(expected.clone()));
        d.finish().unwrap();
    }
    let mut d = Decoder::new();
    let mut received = vec![];
    for byte in bytes {
        let progress = d.feed(&[byte]).unwrap();
        assert_eq!(progress.consumed, 1);
        received.extend(progress.frame);
    }
    assert_eq!(received, vec![expected]);
    d.finish().unwrap();
}

#[test]
fn coalesced_frames_leave_the_next_frame_with_the_caller() {
    let a = encode(&hello()).unwrap();
    let both = [a.as_slice(), a.as_slice()].concat();
    let mut d = Decoder::new();
    let first = d.feed(&both).unwrap();
    assert_eq!(first.consumed, a.len());
    assert_eq!(first.frame, Some(hello()));
    let next = d.feed(&both[first.consumed..]).unwrap();
    assert_eq!(next.consumed, a.len());
    assert_eq!(next.frame, Some(hello()));
    assert_eq!(d.feed(&[]).unwrap().consumed, 0);
    d.finish().unwrap();
}

#[test]
fn every_incomplete_prefix_reports_truncation_and_poison() {
    let bytes = encode(&hello()).unwrap();
    for prefix in 1..bytes.len() {
        let mut d = Decoder::new();
        assert!(d.feed(&bytes[..prefix]).unwrap().frame.is_none());
        assert_eq!(d.finish(), Err(ProtocolError::Truncated));
        assert_eq!(d.feed(&bytes).unwrap_err(), ProtocolError::Poisoned);
        assert_eq!(d.finish(), Err(ProtocolError::Poisoned));
    }
}

#[test]
fn length_validation_happens_at_the_header_and_errors_are_terminal() {
    for length in [0u32, 65_533, u32::MAX] {
        let mut d = Decoder::new();
        assert_eq!(
            d.feed(&length.to_be_bytes()).unwrap_err(),
            ProtocolError::InvalidLength
        );
        assert_eq!(
            d.feed(&encode(&hello()).unwrap()).unwrap_err(),
            ProtocolError::Poisoned
        );
    }
    let body = vec![b'x'; 65_532];
    let mut d = Decoder::new();
    assert_eq!(
        d.feed(&framed(&body)).unwrap_err(),
        ProtocolError::InvalidJson
    );
    // Whitespace is legal JSON: the exact inbound byte limit is accepted.
    let valid = encode(&hello()).unwrap();
    let mut body = vec![b' '; 65_532 - (valid.len() - 4)];
    body.extend_from_slice(&valid[4..]);
    assert_eq!(
        Decoder::new().feed(&framed(&body)).unwrap().frame,
        Some(hello())
    );
}

#[test]
fn wire_rejects_invalid_utf8_json_schema_versions_and_identity() {
    let valid = r#"{"version":1,"session":"07070707070707070707070707070707","sequence":0,"message":{"kind":"hello","role":"controller","protocol_min":1,"protocol_max":1}}"#;
    let cases = [
        (
            valid.replace("\"version\":1", "\"version\":2"),
            ProtocolError::UnsupportedVersion,
        ),
        (
            valid.replace("\"protocol_min\":1", "\"protocol_min\":0"),
            ProtocolError::UnsupportedVersion,
        ),
        (
            valid.replace("\"protocol_max\":1", "\"protocol_max\":2"),
            ProtocolError::UnsupportedVersion,
        ),
        (
            valid.replace("\"sequence\":0", "\"sequence\":-1"),
            ProtocolError::InvalidEnvelope,
        ),
        (
            valid.replace("\"sequence\":0", "\"sequence\":1e0"),
            ProtocolError::InvalidEnvelope,
        ),
        (
            valid.replace("\"sequence\":0", "\"sequence\":18446744073709551616"),
            ProtocolError::InvalidEnvelope,
        ),
        (
            valid.replace("\"role\":\"controller\"", "\"role\":\"unknown\""),
            ProtocolError::InvalidEnvelope,
        ),
        (
            valid.replace("\"version\":1", "\"extra\":true,\"version\":1"),
            ProtocolError::InvalidEnvelope,
        ),
        (
            valid.replace("\"kind\":\"hello\"", "\"extra\":true,\"kind\":\"hello\""),
            ProtocolError::InvalidEnvelope,
        ),
        (
            valid.replace("\"version\":1", "\"version\":1,\"version\":1"),
            ProtocolError::InvalidJson,
        ),
        (
            valid.replace(
                "\"kind\":\"hello\"",
                "\"kind\":\"hello\",\"kind\":\"hello\"",
            ),
            ProtocolError::InvalidJson,
        ),
        (
            valid.replace(
                "07070707070707070707070707070707",
                "ABABABABABABABABABABABABABABABAB",
            ),
            ProtocolError::InvalidSessionId,
        ),
        (format!("{valid} null"), ProtocolError::InvalidJson),
    ];
    for (body, want) in cases {
        let mut d = Decoder::new();
        assert_eq!(
            d.feed(&framed(body.as_bytes())).unwrap_err(),
            want,
            "{body}"
        );
        assert_eq!(d.finish(), Err(ProtocolError::Poisoned));
    }
    assert_eq!(
        Decoder::new().feed(&framed(&[0xff])).unwrap_err(),
        ProtocolError::InvalidUtf8
    );
    let body = valid.replace("\"sequence\":0", "\"sequence\":18446744073709551615");
    assert_eq!(
        Decoder::new()
            .feed(&framed(body.as_bytes()))
            .unwrap()
            .frame
            .unwrap()
            .sequence,
        u64::MAX
    );
}

#[test]
fn helper_hello_and_all_fixed_refusals_round_trip_without_coercion() {
    use theseus_native_protocol::Refusal;
    let mut messages = vec![Message::Hello { role: Role::Helper }];
    messages.extend(
        [
            Refusal::UnsupportedVersion,
            Refusal::SessionMismatch,
            Refusal::UnexpectedSequence,
            Refusal::UnexpectedMessage,
        ]
        .map(|code| Message::Refused { code }),
    );
    for message in messages {
        let envelope = Envelope {
            session: SessionId::from_bytes([0xff; 16]),
            sequence: u64::MAX,
            message,
        };
        assert_eq!(
            Decoder::new()
                .feed(&encode(&envelope).unwrap())
                .unwrap()
                .frame,
            Some(envelope)
        );
    }
}

#[test]
fn externally_authored_wire_messages_define_both_encoding_and_decoding() {
    use theseus_native_protocol::Refusal;
    let cases = [
        (
            r#"{"kind":"hello","role":"helper","protocol_min":1,"protocol_max":1}"#,
            Message::Hello { role: Role::Helper },
        ),
        (
            r#"{"kind":"refused","code":"unsupported_version"}"#,
            Message::Refused {
                code: Refusal::UnsupportedVersion,
            },
        ),
        (
            r#"{"kind":"refused","code":"session_mismatch"}"#,
            Message::Refused {
                code: Refusal::SessionMismatch,
            },
        ),
        (
            r#"{"kind":"refused","code":"unexpected_sequence"}"#,
            Message::Refused {
                code: Refusal::UnexpectedSequence,
            },
        ),
        (
            r#"{"kind":"refused","code":"unexpected_message"}"#,
            Message::Refused {
                code: Refusal::UnexpectedMessage,
            },
        ),
    ];
    for (literal, message) in cases {
        let body = format!(
            r#"{{"version":1,"session":"00000000000000000000000000000000","sequence":0,"message":{literal}}}"#
        );
        let expected = Envelope {
            session: SessionId::from_bytes([0; 16]),
            sequence: 0,
            message,
        };
        assert_eq!(
            Decoder::new().feed(&framed(body.as_bytes())).unwrap().frame,
            Some(expected.clone())
        );
        let encoded = encode(&expected).unwrap();
        let actual: serde_json::Value = serde_json::from_slice(&encoded[4..]).unwrap();
        let reference: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(actual, reference);
    }
}

#[test]
fn unknown_operations_and_missing_or_wrongly_typed_fields_fail_closed() {
    let valid = r#"{"version":1,"session":"00000000000000000000000000000000","sequence":0,"message":{"kind":"hello","role":"controller","protocol_min":1,"protocol_max":1}}"#;
    let bodies = [
        valid.replace(r#""kind":"hello""#, r#""kind":"execute""#),
        valid.replace(
            r#""kind":"hello","role":"controller","protocol_min":1,"protocol_max":1"#,
            r#""kind":"refused","code":"execute""#,
        ),
        valid.replace(
            r#""kind":"hello","role":"controller","protocol_min":1,"protocol_max":1"#,
            r#""kind":"refused""#,
        ),
        valid.replace(r#""version":1,"#, ""),
        valid.replace(r#""version":1"#, r#""version":null"#),
        valid.replace(r#""sequence":0"#, r#""sequence":"0""#),
        valid.replace(r#""sequence":0"#, r#""sequence":true"#),
        valid.replace(
            r#""session":"00000000000000000000000000000000""#,
            r#""session":null"#,
        ),
        valid.replace(r#""kind":"hello","#, ""),
        valid.replace(r#""protocol_min":1"#, r#""protocol_min":"1""#),
        valid.replace(r#","protocol_max":1"#, ""),
        valid.replace(r#""role":"controller""#, r#""role":null"#),
    ];
    for body in bodies {
        assert_eq!(
            Decoder::new().feed(&framed(body.as_bytes())).unwrap_err(),
            ProtocolError::InvalidEnvelope,
            "{body}"
        );
    }
}
