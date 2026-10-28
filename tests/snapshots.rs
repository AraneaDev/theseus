mod support;
use std::io::{self, Read};
use support::{Tar, header};
use theseus::model::FileKind;
use theseus::snapshot::{ChangeKind, Limits, ObservationStatus, diff, read_archive};

fn snapshot(tar: Tar) -> theseus::snapshot::Snapshot {
    read_archive(tar.finish().as_slice(), "/usr/local/bin", Limits::default()).unwrap()
}
#[test]
fn docker_root_is_mapped_and_files_are_hashed_without_saving_content() {
    let snapshot = snapshot(Tar::new().root().file("bin/tool", b"hello"));
    assert_eq!(snapshot.completeness, ObservationStatus::Complete);
    let entry = &snapshot.entries["/usr/local/bin/tool"];
    assert_eq!(
        entry.sha256.as_deref(),
        Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
    );
    assert_eq!(entry.kind, FileKind::File);
    assert_eq!(
        (entry.uid, entry.gid, entry.mode, entry.size),
        (10001, 10001, 0o644, 5)
    );
    assert!(!serde_json::to_string(&snapshot).unwrap().contains("hello"));
}
#[test]
fn diff_is_ordered_and_detects_content_mode_ownership_links_additions_and_removals() {
    let before = snapshot(
        Tar::new()
            .root()
            .file("bin/a", b"old")
            .file("bin/z", b"gone")
            .entry("bin/link", b'2', b"", 0o777, 0, 0, "old"),
    );
    let after = snapshot(
        Tar::new()
            .root()
            .entry("bin/a", b'0', b"new", 0o755, 5, 7, "")
            .file("bin/b", b"new")
            .entry("bin/link", b'2', b"", 0o777, 0, 0, "new"),
    );
    let changes = diff(&before, &after);
    assert_eq!(
        changes
            .iter()
            .map(|c| (c.path.as_str(), c.kind))
            .collect::<Vec<_>>(),
        vec![
            ("/usr/local/bin/a", ChangeKind::Modified),
            ("/usr/local/bin/b", ChangeKind::Added),
            ("/usr/local/bin/link", ChangeKind::Modified),
            ("/usr/local/bin/z", ChangeKind::Removed)
        ]
    );
    for modified in [
        Tar::new()
            .root()
            .entry("bin/a", b'0', b"old", 0o600, 10001, 10001, ""),
        Tar::new()
            .root()
            .entry("bin/a", b'0', b"old", 0o644, 0, 10001, ""),
        Tar::new()
            .root()
            .entry("bin/a", b'0', b"old", 0o644, 10001, 0, ""),
    ] {
        assert_eq!(
            diff(
                &snapshot(Tar::new().root().file("bin/a", b"old")),
                &snapshot(modified)
            )
            .len(),
            1
        );
    }
}
#[test]
fn rejects_absolute_traversal_outside_duplicate_special_and_hardlink_entries() {
    for (name, kind) in [
        ("/bin/a", b'0'),
        ("bin/../a", b'0'),
        ("elsewhere/a", b'0'),
        ("bin/socket", b'6'),
        ("bin/device", b'3'),
        ("bin/hard", b'1'),
    ] {
        let tar = Tar::new()
            .root()
            .entry(name, kind, b"", 0o644, 0, 0, "bin/a")
            .finish();
        assert!(
            read_archive(&tar[..], "/usr/local/bin", Limits::default()).is_err(),
            "accepted {name}/{kind}"
        );
    }
    let tar = Tar::new()
        .root()
        .file("bin/a", b"a")
        .file("./bin/a", b"a")
        .finish();
    assert!(read_archive(&tar[..], "/usr/local/bin", Limits::default()).is_err());
    let tar = Tar::new()
        .root()
        .entry("bin/link", b'2', b"", 0o777, 0, 0, "/outside")
        .file("bin/link/child", b"x")
        .finish();
    assert!(read_archive(&tar[..], "/usr/local/bin", Limits::default()).is_err());
}
#[test]
fn truncated_or_corrupt_archive_is_incomplete_instead_of_empty_success() {
    let mut tar = Tar::new().root().file("bin/a", b"abc").finish();
    tar[0] ^= 1;
    assert!(read_archive(&tar[..], "/usr/local/bin", Limits::default()).is_err());
    let tar = Tar::new().root().file("bin/a", b"abc").finish();
    for length in [0, 100, 1025, 1535] {
        assert!(read_archive(&tar[..length], "/usr/local/bin", Limits::default()).is_err());
    }
    assert_ne!(
        theseus::snapshot::Snapshot::missing("/usr/local/bin").completeness,
        ObservationStatus::Complete
    );
}
struct CountReader {
    inner: io::Cursor<Vec<u8>>,
    count: std::rc::Rc<std::cell::Cell<usize>>,
}
impl Read for CountReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.set(self.count.get() + n);
        Ok(n)
    }
}
#[test]
fn bounds_are_enforced_before_reading_rejected_file_content() {
    let count = std::rc::Rc::new(std::cell::Cell::new(0));
    let reader = CountReader {
        inner: io::Cursor::new(header("bin/a", b'0', 1_000_000, 0o644, 0, 0, "").to_vec()),
        count: count.clone(),
    };
    assert!(
        read_archive(
            reader,
            "/usr/local/bin",
            Limits {
                max_entries: 2,
                max_content_bytes: 4
            }
        )
        .is_err()
    );
    assert_eq!(count.get(), 512);
    let tar = Tar::new()
        .root()
        .file("bin/a", b"123")
        .file("bin/b", b"45")
        .finish();
    assert!(
        read_archive(
            &tar[..],
            "/usr/local/bin",
            Limits {
                max_entries: 2,
                max_content_bytes: 10
            }
        )
        .is_err()
    );
    assert!(
        read_archive(
            &tar[..],
            "/usr/local/bin",
            Limits {
                max_entries: 10,
                max_content_bytes: 4
            }
        )
        .is_err()
    );
}
#[test]
fn supports_pax_paths_and_rejects_pax_traversal() {
    fn pax(path: &str) -> Vec<u8> {
        let payload = format!("path={path}\n");
        let mut length = payload.len() + 2;
        while length != payload.len() + length.to_string().len() + 1 {
            length = payload.len() + length.to_string().len() + 1;
        }
        format!("{length} {payload}").into_bytes()
    }
    let path = format!("bin/{}", "a".repeat(150));
    let tar = Tar::new()
        .root()
        .entry("pax", b'x', &pax(&path), 0, 0, 0, "")
        .file("short", b"x")
        .finish();
    let result = read_archive(&tar[..], "/usr/local/bin", Limits::default()).unwrap();
    assert!(result.entries.contains_key(&format!("/usr/local/{path}")));
    let tar = Tar::new()
        .root()
        .entry("pax", b'x', &pax("bin/../escape"), 0, 0, 0, "")
        .file("short", b"x")
        .finish();
    assert!(read_archive(&tar[..], "/usr/local/bin", Limits::default()).is_err());
}
