use super::*;
#[test]
fn compact_json_range_can_split_utf8_and_escapes() {
    let mut w = RangeWriter::new(1, 3);
    serde_json::to_writer(&mut w, &"é\n").unwrap();
    assert_eq!(w.total_bytes(), 6);
    assert_eq!(w.bytes(), &[0xc3, 0xa9, b'\\']);
}
#[test]
fn actual_writer_retains_only_window_while_counting_large_stream() {
    let mut w = RangeWriter::new(1_000_001, 3);
    let chunk = [b'x'; 4096];
    for _ in 0..1024 {
        w.write_all(&chunk).unwrap();
        assert!(w.bytes.capacity() <= 3);
    }
    assert_eq!(w.total_bytes(), 4_194_304);
    assert_eq!(w.bytes(), b"xxx");
    let mut zero = RangeWriter::new(u64::MAX, 0);
    zero.write_all(b"abc").unwrap();
    assert!(zero.bytes().is_empty());
}
#[test]
fn checked_count_overflow_keeps_previous_window() {
    let mut w = RangeWriter::new(0, 2);
    w.total = u64::MAX;
    assert_eq!(
        w.write(b"x").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(w.total_bytes(), u64::MAX);
    assert!(w.bytes().is_empty());
}
