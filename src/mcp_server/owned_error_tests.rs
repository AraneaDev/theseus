use super::*;
use crate::runner::process::{test_support::PendingFixture, worker_panic};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Debug)]
struct Primary;
impl fmt::Display for Primary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("original primary")
    }
}
impl std::error::Error for Primary {}
struct Witness(Arc<AtomicUsize>);
impl Drop for Witness {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn ordinary_recipe_error_does_not_invent_launch_or_block_admission() {
    let owner = OwnedError::capture(QueryError::new(ErrorCode::RecipeInvalid, Primary.into()));
    assert!(owner.primary().is::<Primary>());
    assert_eq!(owner.view().code, ErrorCode::RecipeInvalid);
    assert_eq!(owner.view().message.text, "original primary");
    assert_eq!(owner.view().native, NativeCleanup::Complete);
    assert_eq!(owner.view().launch, None);
    assert!(!owner.view().cleanup_blocked);
}

#[test]
fn non_string_panic_survives_serialized_and_suppressed_views() {
    let drops = Arc::new(AtomicUsize::new(0));
    let original = worker_panic(Box::new(Witness(drops.clone())));
    let owner = OwnedError::capture(QueryError::new(ErrorCode::WorkerError, original));
    for _ in 0..4 {
        let bytes = serde_json::to_vec(owner.view()).unwrap();
        assert!(!bytes.is_empty());
        drop(bytes); // Cancellation discards the projection, never its authority.
        assert_eq!(drops.load(Ordering::SeqCst), 0);
    }
    drop(owner);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn bounded_formatting_stops_at_utf8_limit_without_formatting_the_entire_error() {
    #[derive(Debug)]
    struct Huge(Arc<AtomicUsize>);
    impl fmt::Display for Huge {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            for _ in 0..1_000_000 {
                self.0.fetch_add(1, Ordering::SeqCst);
                f.write_str("界")?;
            }
            Ok(())
        }
    }
    impl std::error::Error for Huge {}
    let writes = Arc::new(AtomicUsize::new(0));
    let owner = OwnedError::capture(QueryError::new(
        ErrorCode::IoError,
        Huge(writes.clone()).into(),
    ));
    assert_eq!(owner.view().message.text, "界".repeat(170));
    assert!(owner.view().message.truncated);
    assert!(!owner.view().message.lossy);
    assert_eq!(writes.load(Ordering::SeqCst), 171);
}

#[test]
fn io_payload_owner_survives_failed_finalization_and_cached_view_transfer() {
    let fixture = PendingFixture::new();
    // io::Error's source skips the payload itself: get_ref must be inspected.
    let original =
        anyhow::Error::new(std::io::Error::other(fixture.failure.clone())).context(Primary);
    let owner = OwnedError::capture(QueryError::new(ErrorCode::WorkerError, original));
    assert!(owner.primary().is::<Primary>());
    assert_eq!(owner.view().native, NativeCleanup::Pending);
    assert_eq!(owner.view().launch, Some(Launch::MayRun));
    assert!(owner.view().cleanup_blocked);
    let frozen = serde_json::to_vec(owner.view()).unwrap();
    let owner = std::thread::spawn(move || owner.finalize()).join().unwrap();
    assert!(owner.primary().is::<Primary>());
    assert!(owner.primary().is::<CleanupFailures>());
    assert!(owner.view().cleanup_blocked);
    fixture.release_fault();
    let owner = std::thread::spawn(move || owner.finalize()).join().unwrap();
    assert!(owner.primary().is::<Primary>());
    assert_eq!(owner.view().native, NativeCleanup::Complete);
    assert!(!owner.view().cleanup_blocked);
    assert_eq!(owner.view().launch, Some(Launch::MayRun));
    let old: serde_json::Value = serde_json::from_slice(&frozen).unwrap();
    assert_eq!(old["native"], "pending");
    assert_eq!(old["cleanup_blocked"], true);
}

#[test]
fn completed_native_record_preserves_historical_launch_before_pending_filter() {
    let fixture = PendingFixture::new();
    fixture.release_fault();
    while !fixture.failure.retry_cleanup().unwrap() {
        std::thread::yield_now();
    }
    let owner = OwnedError::capture(QueryError::new(
        ErrorCode::WorkerError,
        fixture.failure.clone().into(),
    ));
    assert_eq!(owner.view().native, NativeCleanup::Complete);
    assert_eq!(owner.view().launch, Some(Launch::MayRun));
    assert!(!owner.view().cleanup_blocked);
}

#[test]
fn typed_context_retains_original_panic_and_native_owner_through_failed_serialization() {
    let fixture = PendingFixture::new();
    let mut cleanup = CleanupFailures::default();
    cleanup.retain(&fixture.failure.clone().into());
    let drops = Arc::new(AtomicUsize::new(0));
    let original = worker_panic(Box::new(Witness(drops.clone())))
        .context(cleanup)
        .context(Primary);
    let owner = OwnedError::capture(QueryError::new(ErrorCode::WorkerError, original));
    assert!(owner.primary().is::<Primary>());
    assert!(owner.view().cleanup_blocked);
    struct Full;
    impl std::io::Write for Full {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("response budget exhausted"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(serde_json::to_writer(Full, owner.view()).is_err());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let owner = std::thread::spawn(move || owner.finalize()).join().unwrap();
    assert!(owner.primary().is::<Primary>());
    assert!(owner.view().cleanup_blocked);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    fixture.release_fault();
    let owner = std::thread::spawn(move || owner.finalize()).join().unwrap();
    assert!(!owner.view().cleanup_blocked);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(owner);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[cfg(windows)]
#[test]
fn actual_suspended_windows_owner_is_retained_until_native_discharge() {
    use crate::runner::process::{native_tests, release_test_fault};
    let (temp, failure) = native_tests::suspended_failure();
    let owner = OwnedError::capture(QueryError::new(
        ErrorCode::WorkerError,
        std::io::Error::other(failure.clone()).into(),
    ));
    assert_eq!(owner.view().launch, Some(Launch::CreatedSuspended));
    assert_eq!(owner.view().native, NativeCleanup::Pending);
    assert!(owner.view().cleanup_blocked);
    drop(serde_json::to_vec(owner.view()).unwrap());
    assert!(failure.pending());
    release_test_fault(&failure);
    let owner = std::thread::spawn(move || owner.finalize()).join().unwrap();
    assert_eq!(owner.view().native, NativeCleanup::Complete);
    assert_eq!(owner.view().launch, Some(Launch::CreatedSuspended));
    native_tests::finish_native(&temp, &failure);
}

#[test]
fn serialized_failure_uses_only_cached_projection_and_keeps_typed_primary() {
    use super::super::types::{Failure, Instance};
    let owner = OwnedError::capture(QueryError::new(ErrorCode::WorkerError, Primary.into()));
    let failure = Failure::new(
        Instance::parse(&"a".repeat(32)).unwrap(),
        owner.view().code,
        owner.view().message.clone(),
        None,
    );
    let bytes = serde_json::to_vec(&failure).unwrap();
    assert!(owner.primary().is::<Primary>());
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["error"]["code"], "worker_error");
    assert_eq!(value["error"]["message"]["text"], "original primary");
    assert_eq!(value["error"]["handle"], serde_json::Value::Null);
}

#[test]
fn replacement_characters_in_native_diagnostics_are_marked_lossy() {
    let owner = OwnedError::capture(QueryError::new(
        ErrorCode::UnsafePath,
        anyhow::anyhow!("path: /tmp/\u{fffd}"),
    ));
    assert!(owner.view().message.lossy);
    assert!(!owner.view().message.truncated);
}

#[test]
fn typed_unknown_publication_payload_blocks_even_without_native_launch_fact() {
    // Synthetic classification input, not proof of a pending native operation.
    let original =
        anyhow::Error::new(theseus_host::fs::publication_unknown_fixture_error()).context(Primary);
    let owner = OwnedError::capture(QueryError::new(ErrorCode::IoError, original));
    assert!(owner.primary().is::<Primary>());
    assert_eq!(owner.view().native, NativeCleanup::Unknown);
    assert_eq!(owner.view().launch, None);
    assert!(owner.view().cleanup_blocked);
    let owner = std::thread::spawn(move || owner.finalize()).join().unwrap();
    assert!(owner.primary().is::<Primary>());
    assert_eq!(owner.view().native, NativeCleanup::Unknown);
    assert!(owner.view().cleanup_blocked);
}
