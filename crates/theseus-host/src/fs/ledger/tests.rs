use super::*;
fn owner() -> (tempfile::TempDir, LockedLedgerFile) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("private");
    let root = HostDir::create_private(&path).unwrap();
    std::fs::write(path.join("ledger"), b"old").unwrap();
    let owner = root.open_locked_ledger(OsStr::new("ledger")).unwrap();
    (temp, owner)
}
#[test]
fn partial_write_preserves_native_error_and_permanently_poisons() {
    let (_temp, mut owner) = owner();
    owner.fault = Some(Fault::PartialWrite);
    assert_eq!(
        owner.append_and_sync(b"new", 6).unwrap_err().raw_os_error(),
        Some(5)
    );
    assert_eq!(owner.read_bounded(6).unwrap(), b"oldn");
    assert!(owner.append_and_sync(b"later", 100).is_err());
    assert!(owner.append_and_sync(b"", 100).is_err());
    assert_eq!(owner.read_bounded(6).unwrap(), b"oldn");
}
#[test]
fn failed_sync_preserves_bytes_and_permanently_poisons() {
    let (_temp, mut owner) = owner();
    owner.fault = Some(Fault::Sync);
    assert_eq!(
        owner.append_and_sync(b"new", 6).unwrap_err().raw_os_error(),
        Some(5)
    );
    assert_eq!(owner.read_bounded(6).unwrap(), b"oldnew");
    assert!(owner.append_and_sync(b"later", 100).is_err());
    assert_eq!(owner.read_bounded(6).unwrap(), b"oldnew");
}
#[test]
fn empty_append_still_syncs_and_can_poison() {
    let (_temp, mut owner) = owner();
    owner.fault = Some(Fault::Sync);
    assert_eq!(
        owner.append_and_sync(b"", 3).unwrap_err().raw_os_error(),
        Some(5)
    );
    assert!(owner.append_and_sync(b"x", 4).is_err());
    assert_eq!(owner.read_bounded(3).unwrap(), b"old");
}
#[test]
fn checked_length_overflow_is_refused() {
    assert!(append_length(u64::MAX, 1, u64::MAX).is_err());
    assert_eq!(append_length(3, 1, 4).unwrap(), 4);
}
