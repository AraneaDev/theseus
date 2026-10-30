use std::{
    fs,
    os::unix::fs::{FileExt, symlink},
};
use theseus::{
    model::LogReference,
    report::{self, read_log_page},
};
fn reference(bytes: u64) -> LogReference {
    LogReference {
        path: "logs/stdout".into(),
        bytes,
        truncated: true,
    }
}
#[test]
fn sparse_tail_ignores_unpublished_bytes_and_reports_lengths() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    let file = store.create_log("logs/stdout").unwrap();
    let offset = 16 * 1024 * 1024 - 10;
    file.write_at(b"TAILHIDDEN", offset).unwrap();
    let page = read_log_page(store.root(), &reference(offset + 4), offset, 4096).unwrap();
    assert_eq!(page.bytes, b"TAIL");
    assert_eq!(page.offset, offset);
    assert_eq!(page.next_offset, None);
    assert_eq!(page.current_bytes, offset + 10);
    assert_eq!(page.readable_total, offset + 4);
    assert_eq!(page.recorded_bytes, offset + 4);
}
#[test]
fn pages_are_raw_contiguous_and_short_stream_is_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    let bytes = b"a\xf0\x9f\x90\x88\xff\t\x1bEND";
    store.write_log("logs/stdout", bytes, 100).unwrap();
    let r = reference(100);
    let mut offset = 0;
    let mut output = Vec::new();
    loop {
        let page = store.read_log_page(&r, offset, 3).unwrap();
        assert_eq!(page.readable_total, bytes.len() as u64);
        output.extend(page.bytes);
        if let Some(next) = page.next_offset {
            offset = next;
        } else {
            break;
        }
    }
    assert_eq!(output, bytes);
    assert_eq!(
        store
            .read_log_page(&r, bytes.len() as u64, 1)
            .unwrap()
            .bytes,
        b""
    );
    for (offset, limit) in [
        (0, 0),
        (0, 65537),
        (u64::MAX, 1),
        (bytes.len() as u64 + 1, 1),
    ] {
        assert!(store.read_log_page(&r, offset, limit).is_err());
    }
    assert!(
        store
            .read_log_page(&reference(16 * 1024 * 1024 + 1), 0, 1)
            .is_err()
    );
    for path in [
        "../escape",
        "logs/../stdout",
        "/etc/passwd",
        "report.json",
        "logs/.hidden",
    ] {
        let mut bad = r.clone();
        bad.path = path.into();
        assert!(store.read_log_page(&bad, 0, 1).is_err());
    }
}
#[test]
fn no_follow_regular_files_and_fingerprints() {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    let file = store.create_log("logs/stdout").unwrap();
    file.write_at(b"abc", 0).unwrap();
    let r = reference(3);
    let first = store.read_log_page(&r, 0, 1).unwrap();
    file.write_at(b"d", 3).unwrap();
    let grown = store.read_log_page(&r, 0, 1).unwrap();
    assert_ne!(first.fingerprint, grown.fingerprint);
    fs::rename(
        store.root().join("logs/stdout"),
        store.root().join("logs/old"),
    )
    .unwrap();
    fs::write(store.root().join("logs/stdout"), b"abc").unwrap();
    assert_ne!(
        first.fingerprint,
        store.read_log_page(&r, 0, 1).unwrap().fingerprint
    );
    fs::remove_file(store.root().join("logs/stdout")).unwrap();
    symlink("old", store.root().join("logs/stdout")).unwrap();
    assert!(store.read_log_page(&r, 0, 1).is_err());
    fs::remove_file(store.root().join("logs/stdout")).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        store.root().join("logs/stdout"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR,
        0,
    )
    .unwrap();
    assert!(store.read_log_page(&r, 0, 1).is_err());
    fs::remove_file(store.root().join("logs/stdout")).unwrap();
    fs::remove_file(store.root().join("logs/old")).unwrap();
    fs::remove_dir(store.root().join("logs")).unwrap();
    symlink(temp.path(), store.root().join("logs")).unwrap();
    assert!(store.read_log_page(&r, 0, 1).is_err());
}
#[test]
fn bounded_maximum_page_empty_and_pinned_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("run");
    let store = report::create(&root).unwrap();
    let file = store.create_log("logs/stdout").unwrap();
    file.set_len(100_000).unwrap();
    let page = store.read_log_page(&reference(100_000), 0, 65_536).unwrap();
    assert_eq!(page.bytes.len(), 65_536);
    assert_eq!(page.next_offset, Some(65_536));
    assert_eq!(
        store
            .read_log_page(&reference(0), 0, 4)
            .unwrap()
            .bytes
            .len(),
        0
    );
    fs::rename(&root, temp.path().join("old-run")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("logs")).unwrap();
    fs::write(root.join("logs/stdout"), b"other").unwrap();
    assert_eq!(
        store.read_log_page(&reference(5), 0, 5).unwrap().bytes,
        b"\0\0\0\0\0"
    );
    assert_eq!(
        read_log_page(&root, &reference(5), 0, 5).unwrap().bytes,
        b"other"
    );
}
