use theseus_native_protocol::{ProtocolError, SessionId};

#[test]
fn session_id_preserves_all_bytes_in_canonical_lowercase_hex() {
    let bytes = [
        0x00, 0x01, 0x09, 0x0a, 0x0f, 0x10, 0x7f, 0x80, 0x99, 0xa0, 0xab, 0xcd, 0xef, 0xfe, 0xff,
        0x42,
    ];
    let text = "0001090a0f107f8099a0abcdeffeff42";
    let id = SessionId::from_bytes(bytes);
    assert_eq!(id.to_hex(), text);
    assert_eq!(SessionId::parse(text), Ok(id));
    assert_eq!(
        SessionId::parse("00000000000000000000000000000000"),
        Ok(SessionId::from_bytes([0; 16]))
    );
}

#[test]
fn session_id_rejects_alternate_spellings_and_invalid_widths() {
    // Accepting any of these could alias or reinterpret a pinned session identity.
    for input in [
        "",
        "00",
        "0000000000000000000000000000000",
        "000000000000000000000000000000000",
        "ABABABABABABABABABABABABABABABAB",
        "gggggggggggggggggggggggggggggggg",
        " 0000000000000000000000000000000",
        "0000000000000000000000000000000\n",
        "００００００００００００００００００００００００００００００００００００００",
        "0000000000000000000000000000000é",
        "0000000000000000000000000000000\0",
    ] {
        assert_eq!(
            SessionId::parse(input),
            Err(ProtocolError::InvalidSessionId),
            "{input:?}"
        );
    }
}

#[test]
fn session_id_accepts_each_byte_value_without_loss() {
    for byte in u8::MIN..=u8::MAX {
        let id = SessionId::from_bytes([byte; 16]);
        let text = id.to_hex();
        assert_eq!(text.len(), 32);
        assert!(
            text.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(SessionId::parse(&text), Ok(id));
    }
}

#[test]
fn rejected_session_diagnostics_are_bounded_and_do_not_echo_peer_input() {
    use std::error::Error;
    for input in [
        "peer-secret-credential\n\u{1b}[2J",
        &"sensitive-peer-input".repeat(1000),
    ] {
        let error = SessionId::parse(input).unwrap_err();
        let diagnostic = error.to_string();
        assert!(!diagnostic.is_empty() && diagnostic.len() <= 64);
        assert!(diagnostic.bytes().all(|byte| (b' '..=b'~').contains(&byte)));
        assert!(
            !diagnostic.contains("peer-secret") && !diagnostic.contains("sensitive-peer-input")
        );
        assert!(error.source().is_none());
    }
}
