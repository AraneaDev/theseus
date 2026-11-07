//! Closed inspect-failure classification on the existing refusal reply.
use theseus_native_protocol::v2::*;

fn framed(body: &[u8]) -> Vec<u8> {
    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(body);
    frame
}
fn refused(code: RefusalCode, inspect_failure: Option<InspectFailure>) -> Envelope {
    Envelope::new(
        SessionId::from_bytes([1; 16]),
        3,
        Message::Response {
            reply_to: 2,
            result: Reply::Refused {
                code,
                inspect_failure,
            },
        },
    )
    .unwrap()
}
fn body(envelope: &Envelope) -> serde_json::Value {
    serde_json::from_slice(&encode(envelope).unwrap()[4..]).unwrap()
}
fn decode(value: &serde_json::Value) -> Result<Envelope, Error> {
    let frame = framed(&serde_json::to_vec(value).unwrap());
    let mut decoder = Decoder::new();
    decoder
        .feed(&frame)
        .map(|p| p.frame.expect("complete frame"))
}

#[test]
fn every_inspect_failure_round_trips_exactly_on_ownership_pending() {
    let mut names = std::collections::BTreeSet::new();
    for failure in InspectFailure::ALL {
        assert!(names.insert(failure.name()), "unique wire name");
        assert!(
            failure
                .name()
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_'),
            "fixed snake_case name only"
        );
        let envelope = refused(RefusalCode::OwnershipPending, Some(failure));
        let json = body(&envelope);
        assert_eq!(json["message"]["result"]["inspect_failure"], failure.name());
        assert_eq!(decode(&json).unwrap(), envelope);
        let frame = encode(&envelope).unwrap();
        let mut decoder = Decoder::new();
        assert_eq!(decoder.feed(&frame).unwrap().frame.unwrap(), envelope);
    }
    assert_eq!(names.len(), InspectFailure::ALL.len());
}

#[test]
fn unclassified_refusals_keep_their_exact_previous_wire_form() {
    let json = body(&refused(RefusalCode::OwnershipPending, None));
    assert_eq!(
        json["message"]["result"],
        serde_json::json!({"kind": "refused", "code": "ownership_pending"})
    );
}

#[test]
fn inspect_failure_carries_no_text_and_only_with_ownership_pending() {
    let base = body(&refused(
        RefusalCode::OwnershipPending,
        Some(InspectFailure::ManagerQuery),
    ));
    // Raw native text, codes, paths or structures are never representable.
    for raw in [
        serde_json::json!("Access is denied. (os error 5)"),
        serde_json::json!("C:\\theseus\\reader.exe"),
        serde_json::json!(1060),
        serde_json::json!({"failure": "manager_query"}),
        serde_json::json!(["manager_query"]),
        serde_json::json!(null),
        serde_json::json!("Manager_Query"),
        serde_json::json!(""),
    ] {
        let mut bad = base.clone();
        bad["message"]["result"]["inspect_failure"] = raw;
        assert!(decode(&bad).is_err());
    }
    for code in ["busy", "unsupported", "invalid_request", "wrong_lease"] {
        let mut bad = base.clone();
        bad["message"]["result"]["code"] = code.into();
        assert!(decode(&bad).is_err(), "{code} cannot carry inspect_failure");
    }
    let mut extra = base;
    extra["message"]["result"]["detail"] = "native text".into();
    assert!(decode(&extra).is_err());
}
