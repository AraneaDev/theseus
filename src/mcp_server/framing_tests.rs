use super::super::{budget::encode, types::*};
use super::*;
fn frame(id: &RpcId) -> OutboundFrame {
    encode(
        Era::Legacy,
        id,
        &Instance::parse(&"a".repeat(32)).unwrap(),
        None,
        &Err(Failure::new(
            Instance::parse(&"a".repeat(32)).unwrap(),
            ErrorCode::InvalidArguments,
            TextPreview::new("bad", false),
            None,
        )),
    )
    .unwrap()
}
#[test]
fn ingress_stops_at_first_newline_and_never_queues_a_second_frame() {
    let input = b"{}\n{}\n";
    let mut d = FrameDecoder::default();
    let n = d.push(input).unwrap();
    assert_eq!(n, 3);
    assert_eq!(d.push(b"x"), Err(FrameError::FrameReady));
    assert_eq!(d.take_frame().unwrap(), b"{}\n");
    assert!(d.take_frame().is_none());
    assert_eq!(d.push(&input[n..]).unwrap(), 3);
    assert_eq!(d.take_frame().unwrap(), b"{}\n");
}
#[test]
fn every_split_preserves_one_frame_and_eof_discards_partial() {
    let input = "{\"text\":\"é😀\\n\"}\n".as_bytes();
    for split in 0..input.len() {
        let mut d = FrameDecoder::default();
        assert_eq!(d.push(&input[..split]).unwrap(), split);
        assert!(d.take_frame().is_none());
        assert_eq!(d.push(&input[split..]).unwrap(), input.len() - split);
        assert_eq!(d.take_frame().unwrap(), input);
    }
    let mut d = FrameDecoder::default();
    d.push(b"partial").unwrap();
    d.eof();
    assert!(d.take_frame().is_none());
    d.push(b"{}\n").unwrap();
    assert_eq!(d.take_frame().unwrap(), b"{}\n");
}
#[test]
fn exact_ingress_cap_includes_the_newline() {
    let mut valid = vec![b' '; 65535];
    valid.push(b'\n');
    let mut d = FrameDecoder::default();
    assert_eq!(d.push(&valid).unwrap(), 65536);
    assert_eq!(d.take_frame().unwrap(), valid);
    let mut d = FrameDecoder::default();
    assert_eq!(d.push(&vec![b' '; 65536]), Err(FrameError::TooLarge));
    let mut invalid = vec![b' '; 65536];
    invalid.push(b'\n');
    assert_eq!(
        FrameDecoder::default().push(&invalid),
        Err(FrameError::TooLarge)
    );
}
#[test]
fn fifth_request_has_no_reserved_error_slot() {
    let mut s = Slots::default();
    for id in 0..4 {
        s.reserve(RpcId::Integer(id)).unwrap();
    }
    assert_eq!(s.reserve(RpcId::Integer(4)), Err(SlotError::Full));
    assert_eq!(s.reserve(RpcId::Integer(0)), Err(SlotError::DuplicateId));
}
#[test]
fn slots_bind_exact_ids_generations_and_actual_positive_write_progress() {
    let mut s = Slots::default();
    let id = RpcId::String("1".into());
    let slot = s.reserve(id.clone()).unwrap();
    let other = s.reserve(RpcId::Integer(1)).unwrap();
    assert_eq!(s.advance(slot, 1), Err(SlotError::InvalidProgress));
    assert_eq!(
        s.stage(slot, frame(&RpcId::Integer(1))),
        Err(SlotError::InvalidProgress)
    );
    let f = frame(&id);
    let len = f.bytes.len();
    s.stage(slot, f).unwrap();
    assert_eq!(s.advance(slot, 0), Err(SlotError::InvalidProgress));
    assert_eq!(s.advance(slot, len + 1), Err(SlotError::InvalidProgress));
    assert!(!s.advance(slot, 1).unwrap());
    assert_eq!(s.frame(slot).unwrap().unwrap().offset, 1);
    assert_eq!(s.cancel(&id), CancelWrite::ClosePartial(slot));
    assert!(s.advance(slot, len - 1).unwrap());
    assert_eq!(s.frame(slot).unwrap_err(), SlotError::StaleSlot);
    let newer = s.reserve(id.clone()).unwrap();
    assert_ne!(newer.generation, slot.generation);
    assert_eq!(s.stage(slot, frame(&id)), Err(SlotError::StaleSlot));
    assert_eq!(s.cancel(&id), CancelWrite::Removed(newer));
    assert_eq!(s.cancel(&id), CancelWrite::Absent);
    assert_eq!(s.cancel(&RpcId::Integer(1)), CancelWrite::Removed(other));
    s.generation = u64::MAX;
    assert_eq!(s.reserve(id), Err(SlotError::GenerationExhausted));
}
#[test]
fn queued_zero_offset_can_cancel_but_staging_cannot_replace_it() {
    let mut s = Slots::default();
    let id = RpcId::Integer(3);
    let slot = s.reserve(id.clone()).unwrap();
    s.stage(slot, frame(&id)).unwrap();
    assert_eq!(s.stage(slot, frame(&id)), Err(SlotError::InvalidProgress));
    assert_eq!(s.cancel(&id), CancelWrite::Removed(slot));
    assert_eq!(
        s.advance(
            SlotId {
                index: 255,
                generation: 1
            },
            1
        ),
        Err(SlotError::StaleSlot)
    );
}

#[test]
fn anonymous_errors_share_four_slots_and_cannot_alias_empty_string_ids() {
    let mut s = Slots::default();
    let anonymous = s.reserve_anonymous().unwrap();
    let empty = RpcId::String(String::new());
    let real = s.reserve(empty.clone()).unwrap();
    assert_eq!(s.reserve(empty.clone()), Err(SlotError::DuplicateId));
    s.reserve(RpcId::Integer(2)).unwrap();
    s.reserve(RpcId::Integer(3)).unwrap();
    assert_eq!(s.reserve_anonymous(), Err(SlotError::Full));
    assert_eq!(s.reserve(RpcId::Integer(4)), Err(SlotError::Full));
    let error = ProtocolError {
        code: -32600,
        message: TextPreview::new("bad", false),
        data: None,
        id: ReplyId::Null,
        close_after_send: true,
    };
    let anonymous_frame = super::super::budget::encode_protocol_error(&error).unwrap();
    assert!(anonymous_frame.id.is_none());
    s.stage(anonymous, anonymous_frame).unwrap();
    assert_eq!(s.cancel(&empty), CancelWrite::Removed(real));
    assert!(s.frame(anonymous).unwrap().is_some());
    let error = ProtocolError {
        code: -32600,
        message: TextPreview::new("bad", false),
        data: None,
        id: ReplyId::Omit,
        close_after_send: true,
    };
    let real = s.reserve(empty.clone()).unwrap();
    assert_eq!(
        s.stage(
            real,
            super::super::budget::encode_protocol_error(&error).unwrap()
        ),
        Err(SlotError::InvalidProgress)
    );
    assert_eq!(
        s.stage(anonymous, frame(&empty)),
        Err(SlotError::InvalidProgress)
    );
}
