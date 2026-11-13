use super::*;
use std::{io::Write, os::unix::fs::PermissionsExt, path::Path};
fn material(path: &Path) -> LaunchPreparation {
    let root = theseus_host::fs::HostDir::create_private(path).unwrap();
    let mut file = root
        .create_file(&theseus_host::fs::RelativePath::parse("identity").unwrap())
        .unwrap();
    file.write_all(b"synthetic key bytes").unwrap();
    file.sync_all().unwrap();
    LaunchPreparation {
        command: None,
        probe: None,
        material: Some(root),
        material_files: vec![("identity".into(), file)],
        error: None,
        launch_fingerprint: "00".repeat(32),
        cleanup_error: None,
        settled_cleanup_history: None,
        settled_cleanup_attempts: 0,
        next_cleanup_retry: None,
    }
}
fn deny_delete(preparation: &mut LaunchPreparation, path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = preparation.poll_settle();
    // Restore immediately even if the expected permission denial fails.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        !result.unwrap(),
        "actual unlink must fail under unprivileged directory0500"
    );
    assert_eq!(
        preparation.settled_cleanup_attempts, 1,
        "one deletion attempt per poll"
    );
    assert_eq!(
        preparation.settled_cleanup_error().unwrap().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(preparation.cleanup_error().is_none());
    assert!(!preparation.is_settled());
}
#[test]
fn settled_auth_delete_retries_same_original_leaf_and_keeps_first_error_history() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("auth");
    let mut preparation = material(&path);
    deny_delete(&mut preparation, &path);
    let first = preparation.settled_cleanup_error().unwrap().raw_os_error();
    assert!(path.join("identity").exists());
    assert_eq!(preparation.material_files.len(), 1);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !preparation.poll_settle().unwrap() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(preparation.is_settled());
    assert!(!path.join("identity").exists());
    assert!(preparation.cleanup_error().is_none());
    assert_eq!(
        preparation.settled_cleanup_error().unwrap().raw_os_error(),
        first
    );
    assert_eq!(preparation.settled_cleanup_attempts, 1);
}
#[test]
fn transient_auth_delete_followed_by_replacement_stays_revoked_after_original_returns() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("auth");
    let mut preparation = material(&path);
    deny_delete(&mut preparation, &path);
    std::fs::rename(path.join("identity"), path.join("original")).unwrap();
    std::fs::write(path.join("identity"), b"synthetic key bytes").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert!(preparation.poll_settle().is_err());
    assert!(preparation.cleanup_error().is_some());
    std::fs::remove_file(path.join("identity")).unwrap();
    std::fs::rename(path.join("original"), path.join("identity")).unwrap();
    assert!(preparation.poll_settle().is_err());
    assert!(!preparation.is_settled());
    assert!(path.join("identity").exists());
    assert_eq!(preparation.material_files.len(), 1);
    assert!(preparation.settled_cleanup_error().is_some());
}
