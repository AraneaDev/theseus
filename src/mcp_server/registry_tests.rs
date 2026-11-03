use super::super::{
    budget::{EncodeFailure, encode},
    cursor::CursorRecord,
};
use super::*;
use crate::{
    model::{ResultCounts, RunOutcome},
    runner::process::{test_support::PendingFixture, worker_panic},
};
use serde_json::json;
use std::{
    fmt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use theseus_host::fs::RelativePath;

fn instance() -> Instance {
    Instance::parse("00000000000000000000000000000001").unwrap()
}
fn args(key: &str) -> StartArgs {
    serde_json::from_value(json!({"server_instance":instance(), "request_key":key,"recipe_path":"theseus.toml","recipe_digest":"0".repeat(64),"profiles":["user"],"scenarios":["clean"]})).unwrap()
}
fn reserve(registry: &mut Registry, key: &str) -> Handle {
    match registry.admit(args(key), true).unwrap() {
        AdmissionDecision::Reserved { handle, validation } => {
            assert_eq!(validation.request_key.as_str(), key);
            handle
        }
        _ => panic!("fresh key must reserve"),
    }
}
fn start(registry: &mut Registry, handle: &Handle, generation: u64) {
    registry
        .apply(handle, RegistryUpdate::ValidationStarted { generation })
        .unwrap();
}
fn recipe() -> ValidatedRecipe {
    // Existing parser validates exact fixture bytes; only fixture construction
    // touches disk. Every registry operation itself is pure and barrier-free.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/recipes/v2_clean.toml")
        .canonicalize()
        .unwrap();
    crate::recipe::parse_bytes(
        &path,
        include_bytes!("../../tests/fixtures/recipes/v2_clean.toml"),
    )
    .unwrap()
}
fn ready(registry: &mut Registry, key: &str) -> Handle {
    let h = reserve(registry, key);
    start(registry, &h, 1);
    registry
        .apply(
            &h,
            RegistryUpdate::Validation {
                generation: 1,
                result: Ok(recipe()),
            },
        )
        .unwrap();
    h
}
fn running(registry: &mut Registry, key: &str) -> Handle {
    let h = ready(registry, key);
    let dispatch = registry.take_dispatch(&h).unwrap();
    assert_eq!(dispatch.handle, h);
    registry.apply(&h, RegistryUpdate::Dispatched).unwrap();
    h
}
fn owner(code: ErrorCode) -> OwnedError {
    OwnedError::capture(QueryError::new(code, Primary.into()))
}
#[derive(Debug)]
struct Primary;
impl fmt::Display for Primary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("original primary")
    }
}
impl std::error::Error for Primary {}
fn snapshot(outcome: RunOutcome, hint: u64, guest: GuestCleanup) -> SavedProjection {
    SavedProjection {
        saved: SavedView {
            native_dispositions: None,
            revision: Revision::parse(&"a".repeat(64)).unwrap(),
            schema_version: 8,
            outcome,
            counts: super::super::status_projection::counts(&ResultCounts::default()).unwrap(),
            finished_at: None,
        },
        guest,
        sampled_hint: hint,
    }
}
fn binding(h: &Handle, id: &str, hint: u64) -> SavedBinding {
    SavedBinding {
        handle: h.clone(),
        report_id: id.into(),
        child: RelativePath::parse(id).unwrap(),
        hint_sequence: hint,
    }
}
fn finish(h: &Handle, outcome: RunOutcome) -> RegistryUpdate {
    RegistryUpdate::Joined(EngineCompletion {
        handle: h.clone(),
        result: Ok(FinalSummary {
            outcome,
            counts: ResultCounts::default(),
            report_path: PathBuf::from("/output/run/report.json"),
            journal_path: PathBuf::from("/output/run/journal.json"),
        }),
    })
}
fn list_args(limit: Option<u64>, cursor: Option<Cursor>) -> ListArgs {
    ListArgs {
        server_instance: instance(),
        limit: limit.map(|n| UInt::new(n).unwrap()),
        cursor,
    }
}
fn reply() -> ReplyContext {
    ReplyContext {
        era: Era::Modern,
        id: RpcId::String("x".repeat(128)),
        instance: instance(),
    }
}
fn frozen(r: &Registry, h: &Handle) -> serde_json::Value {
    serde_json::to_value(r.cached(h).unwrap()).unwrap()
}

#[test]
fn replay_precedes_busy_cleanup_query_and_full_and_retains_terminal_records() {
    let mut r = Registry::new(instance());
    let first = reserve(&mut r, "first");
    r.set_cleanup_blocked(true);
    let AdmissionDecision::Replay(data) = r.admit(args("first"), false).unwrap() else {
        panic!("replay reserved twice")
    };
    assert_eq!(data.handle, first);
    assert!(data.replayed);
    assert_eq!(data.state, RunState::Validating);
    assert_eq!(data.poll_after_ms, 500);
    assert_eq!(data.lifetime, Lifetime::ServerProcess);
    r.set_cleanup_blocked(false);
    r.cancel(&first, CancellationReason::Client).unwrap();
    for n in 2..=128 {
        let h = reserve(&mut r, &format!("key-{n}"));
        assert_eq!(h.as_str(), format!("{}:{n}", instance().as_str()));
        r.cancel(&h, CancellationReason::Client).unwrap();
    }
    let AdmissionDecision::Replay(data) = r.admit(args("first"), false).unwrap() else {
        panic!()
    };
    assert_eq!(data.handle, first);
    assert_eq!(data.state, RunState::StartError);
    assert!(data.cancellation_requested);
    assert_eq!(
        r.admit(args("overflow"), false).unwrap_err().error.code,
        ErrorCode::RegistryFull
    );
    assert_eq!(
        r.list(&list_args(None, None), &reply()).unwrap().high_water,
        128
    );
}
#[test]
fn fingerprint_is_typed_defaulted_and_preserves_every_selection_order() {
    let mut r = Registry::new(instance());
    reserve(&mut r, "a");
    let mut explicit = args("a");
    explicit.pull = false;
    explicit.log_limit = UInt::new(16_777_216).unwrap();
    assert!(matches!(
        r.admit(explicit, false),
        Ok(AdmissionDecision::Replay(_))
    ));
    let mut changes = Vec::new();
    let mut a = args("a");
    a.recipe_path = "other.toml".into();
    changes.push(a);
    let mut a = args("a");
    a.recipe_digest = Digest::parse(&"1".repeat(64)).unwrap();
    changes.push(a);
    let mut a = args("a");
    a.pull = true;
    changes.push(a);
    let mut a = args("a");
    a.log_limit = UInt::new(1).unwrap();
    changes.push(a);
    let mut a = args("a");
    a.profiles = vec![crate::model::ProfileName::System];
    changes.push(a);
    let mut a = args("a");
    a.scenarios = vec![crate::model::ScenarioKind::Upgrade];
    changes.push(a);
    for a in changes {
        assert_eq!(
            r.admit(a, false).unwrap_err().error.code,
            ErrorCode::IdempotencyConflict
        );
    }
    for field in ["profiles", "scenarios"] {
        let mut r = Registry::new(instance());
        let mut value = serde_json::to_value(args("a")).unwrap();
        value[field] = if field == "profiles" {
            json!(["user", "system"])
        } else {
            json!(["clean", "upgrade"])
        };
        let original: StartArgs = serde_json::from_value(value.clone()).unwrap();
        r.admit(original, true).unwrap();
        value[field].as_array_mut().unwrap().reverse();
        assert_eq!(
            r.admit(serde_json::from_value(value).unwrap(), true)
                .unwrap_err()
                .error
                .code,
            ErrorCode::IdempotencyConflict
        );
    }
}
#[test]
fn new_key_rejection_precedence_and_invalid_calls_never_reserve() {
    let mut r = Registry::new(instance());
    r.set_cleanup_blocked(true);
    assert_eq!(
        r.admit(args("new"), false).unwrap_err().error.code,
        ErrorCode::CleanupPending
    );
    r.set_cleanup_blocked(false);
    let h = reserve(&mut r, "active");
    assert_eq!(
        r.admit(args("new"), false).unwrap_err().error.code,
        ErrorCode::Busy
    );
    r.cancel(&h, CancellationReason::Client).unwrap();
    assert_eq!(
        r.admit(args("new"), false).unwrap_err().error.code,
        ErrorCode::QueryBusy
    );
    let mut bad = args("bad");
    bad.scenarios.clear();
    assert_eq!(
        r.admit(bad, true).unwrap_err().error.code,
        ErrorCode::InvalidArguments
    );
    let mut stale = args("bad");
    stale.server_instance = Instance::parse(&"2".repeat(32)).unwrap();
    assert_eq!(
        r.admit(stale, true).unwrap_err().error.code,
        ErrorCode::StaleInstance
    );
    assert_eq!(
        r.list(&list_args(None, None), &reply()).unwrap().high_water,
        1
    );
    let foreign = Handle::parse(&format!("{}:1", "2".repeat(32))).unwrap();
    assert_eq!(
        r.cached(&foreign).unwrap_err().error.code,
        ErrorCode::InvalidArguments
    );
    let unknown = Handle::parse(&format!("{}:2", instance().as_str())).unwrap();
    assert_eq!(
        r.cached(&unknown).unwrap_err().error.code,
        ErrorCode::UnknownRun
    );
    assert_eq!(
        r.cancel(&unknown, CancellationReason::Client)
            .unwrap_err()
            .error
            .code,
        ErrorCode::UnknownRun
    );
}
#[test]
fn cancelled_or_expired_validation_never_dispatches_and_first_reason_wins() {
    for expire in [false, true] {
        let mut r = Registry::new(instance());
        let h = reserve(&mut r, "a");
        start(&mut r, &h, 8);
        assert!(
            r.apply(
                &h,
                RegistryUpdate::Validation {
                    generation: 7,
                    result: Ok(recipe())
                }
            )
            .is_err()
        );
        if expire {
            r.apply(
                &h,
                RegistryUpdate::ValidationExpired {
                    generation: 8,
                    code: ErrorCode::QueryTimeout,
                },
            )
            .unwrap();
        } else {
            let data = r.cancel(&h, CancellationReason::RequestCancelled).unwrap();
            assert!(data.requested_now);
            assert_eq!(
                data.run.cancellation_reason,
                Some(CancellationReason::RequestCancelled)
            );
        }
        let before = frozen(&r, &h);
        assert!(
            !r.cancel(&h, CancellationReason::Client)
                .unwrap()
                .requested_now
        );
        assert_eq!(frozen(&r, &h), before);
        assert!(
            r.apply(
                &h,
                RegistryUpdate::Validation {
                    generation: 8,
                    result: Ok(recipe())
                }
            )
            .is_err()
        );
        assert!(r.take_dispatch(&h).is_none());
        assert_eq!(r.cached(&h).unwrap().state, RunState::StartError);
        assert!(r.cached(&h).unwrap().engine_result.is_none());
        assert_eq!(
            r.cached(&h).unwrap().error.as_ref().unwrap().code,
            if expire {
                ErrorCode::QueryTimeout
            } else {
                ErrorCode::CancelledBeforeStart
            }
        );
    }
}
#[test]
fn admitted_cancellation_cancels_transferred_token_and_dispatch_is_single_use() {
    let mut r = Registry::new(instance());
    let h = ready(&mut r, "a");
    assert_eq!(r.cached(&h).unwrap().state, RunState::Admitted);
    let dispatch = r.take_dispatch(&h).unwrap();
    assert_eq!(dispatch.options.profiles, args("a").profiles);
    assert_eq!(dispatch.options.scenarios, args("a").scenarios);
    assert_eq!(dispatch.options.log_limit, 16_777_216);
    assert!(!dispatch.options.pull);
    assert_eq!(dispatch.recipe.digest, recipe().digest);
    assert!(r.take_dispatch(&h).is_none());
    r.cancel(&h, CancellationReason::ServerShutdown).unwrap();
    assert!(dispatch.token.is_cancelled());
    assert!(r.apply(&h, RegistryUpdate::Dispatched).is_err());
}
#[test]
fn running_starts_before_binding_and_only_join_publishes_actual_outcome() {
    let mut r = Registry::new(instance());
    let h = running(&mut r, "a");
    let run = r.cached(&h).unwrap();
    assert_eq!(run.state, RunState::Running);
    assert_eq!(run.cleanup.native, NativeCleanup::InProgress);
    assert_eq!(run.cleanup.guest, GuestCleanup::InProgress);
    assert!(run.saved.is_none());
    assert!(run.report_id.is_none());
    assert!(r.binding(&h).unwrap().is_none());
    r.apply(&h, RegistryUpdate::Bound(binding(&h, "actual-run", 3)))
        .unwrap();
    assert_eq!(r.binding(&h).unwrap().unwrap().report_id, "actual-run");
    r.apply(
        &h,
        RegistryUpdate::Refreshed(snapshot(RunOutcome::Passed, 2, GuestCleanup::Complete)),
    )
    .unwrap();
    assert_eq!(
        r.cached(&h).unwrap().cleanup.guest,
        GuestCleanup::InProgress
    );
    assert_eq!(r.cached(&h).unwrap().state, RunState::Running);
    assert_eq!(
        r.cached(&h).unwrap().snapshot_hint_sequence.unwrap().get(),
        2
    );
    assert_eq!(r.cached(&h).unwrap().saved_hint_sequence.get(), 3);
    let cancellation = r.cancel(&h, CancellationReason::RuntimeLimit).unwrap();
    assert!(cancellation.requested_now);
    assert_eq!(cancellation.run.state, RunState::Running);
    assert!(
        !r.cancel(&h, CancellationReason::Client)
            .unwrap()
            .requested_now
    );
    assert_eq!(
        r.cached(&h).unwrap().cancellation_reason,
        Some(CancellationReason::RuntimeLimit)
    );
    r.apply(&h, finish(&h, RunOutcome::Failed)).unwrap();
    let run = r.cached(&h).unwrap();
    assert_eq!(run.state, RunState::Finished);
    assert_eq!(
        run.engine_result.as_ref().unwrap().outcome,
        RunOutcome::Failed
    );
    assert_eq!(run.saved.as_ref().unwrap().outcome, RunOutcome::Passed);
    assert_eq!(run.cleanup.native, NativeCleanup::Complete);
    assert_eq!(run.cleanup.guest, GuestCleanup::Complete);
    assert!(run.error.is_none());
    assert!(run.report_path.is_some());
    r.apply(
        &h,
        RegistryUpdate::Refreshed(snapshot(RunOutcome::Cancelled, 3, GuestCleanup::Failed)),
    )
    .unwrap();
    assert_eq!(
        r.cached(&h)
            .unwrap()
            .engine_result
            .as_ref()
            .unwrap()
            .outcome,
        RunOutcome::Failed
    );
    assert_eq!(r.cached(&h).unwrap().cleanup.guest, GuestCleanup::Failed);
    assert!(r.cached(&h).unwrap().cleanup.recovery_required);
    let before = frozen(&r, &h);
    assert!(
        !r.cancel(&h, CancellationReason::Client)
            .unwrap()
            .requested_now
    );
    assert_eq!(frozen(&r, &h), before);
    let frame = encode(
        Era::Modern,
        &reply().id,
        &instance(),
        Some(&h),
        &Ok(ToolData::RunStatus(StatusData {
            run: r.cached(&h).unwrap().clone(),
            refresh: Refresh::Cached,
            poll_after_ms: 500,
        })),
    )
    .unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&frame.bytes).unwrap();
    assert_eq!(envelope["result"]["isError"], false);
    assert_eq!(envelope["result"]["structuredContent"]["ok"], true);
    assert_eq!(
        envelope["result"]["structuredContent"]["data"]["run"]["engine_result"]["outcome"],
        "failed"
    );
}
#[test]
fn binding_identity_and_hint_ranges_are_checked_without_losing_original() {
    let mut r = Registry::new(instance());
    let h = running(&mut r, "a");
    assert!(
        r.apply(
            &h,
            RegistryUpdate::Refreshed(snapshot(RunOutcome::Passed, 0, GuestCleanup::Complete))
        )
        .is_err()
    );
    r.apply(&h, RegistryUpdate::Bound(binding(&h, "original", 1)))
        .unwrap();
    let before = frozen(&r, &h);
    for b in [
        binding(&h, "different", 2),
        binding(&h, "original", 0),
        binding(&h, "original", UINT_MAX + 1),
    ] {
        assert!(r.apply(&h, RegistryUpdate::Bound(b)).is_err());
        assert_eq!(frozen(&r, &h), before);
    }
    r.apply(&h, RegistryUpdate::Bound(binding(&h, "original", UINT_MAX)))
        .unwrap();
    assert_eq!(r.cached(&h).unwrap().saved_hint_sequence.get(), UINT_MAX);
    assert!(
        r.apply(
            &h,
            RegistryUpdate::Refreshed(snapshot(
                RunOutcome::Passed,
                UINT_MAX + 1,
                GuestCleanup::Complete
            ))
        )
        .is_err()
    );
    assert!(
        r.apply(
            &h,
            RegistryUpdate::Refreshed(snapshot(
                RunOutcome::Passed,
                UINT_MAX,
                GuestCleanup::Complete
            ))
        )
        .is_ok()
    );
    let wrong = Handle::parse(&format!("{}:2", instance().as_str())).unwrap();
    assert!(
        r.apply(
            &h,
            RegistryUpdate::Bound(binding(&wrong, "original", UINT_MAX))
        )
        .is_err()
    );
}
#[test]
fn spawn_error_requires_dispatch_attempt_and_does_not_invent_worker_completion() {
    let mut r = Registry::new(instance());
    let h = ready(&mut r, "a");
    assert!(
        r.apply(
            &h,
            RegistryUpdate::SpawnFailed(owner(ErrorCode::WorkerError))
        )
        .is_err()
    );
    r.take_dispatch(&h).unwrap();
    r.apply(
        &h,
        RegistryUpdate::SpawnFailed(owner(ErrorCode::WorkerError)),
    )
    .unwrap();
    let run = r.cached(&h).unwrap();
    assert_eq!(run.state, RunState::WorkerError);
    assert!(run.report_id.is_none());
    assert!(run.engine_result.is_none());
    assert_eq!(run.cleanup.guest, GuestCleanup::NotStarted);
    assert_eq!(run.cleanup.launch, None);
    assert!(r.apply(&h, finish(&h, RunOutcome::Passed)).is_err());
    let owners = r.take_error_owners_for_shutdown();
    assert_eq!(owners.len(), 1);
    assert!(owners[0].primary().is::<Primary>());
}
#[test]
fn rejected_updates_return_original_panic_payload_and_primary_ownership() {
    struct Witness(Arc<AtomicUsize>);
    impl Drop for Witness {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let owned = OwnedError::capture(QueryError::new(
        ErrorCode::WorkerError,
        worker_panic(Box::new(Witness(drops.clone()))).context(Primary),
    ));
    let mut r = Registry::new(instance());
    let h = reserve(&mut r, "a");
    start(&mut r, &h, 2);
    let rejected = r
        .apply(
            &h,
            RegistryUpdate::Validation {
                generation: 1,
                result: Err(owned),
            },
        )
        .unwrap_err();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let RegistryUpdate::Validation {
        result: Err(owned), ..
    } = rejected
    else {
        panic!()
    };
    assert!(owned.primary().is::<Primary>());
    r.apply(
        &h,
        RegistryUpdate::Validation {
            generation: 2,
            result: Err(owned),
        },
    )
    .unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let before = frozen(&r, &h);
    let owners = r.take_error_owners_for_shutdown();
    assert_eq!(owners.len(), 1);
    assert_eq!(frozen(&r, &h), before);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(owners);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(r.take_error_owners_for_shutdown().is_empty());
}
#[test]
fn entry_native_pending_survives_global_gate_clear_and_shutdown_transfer() {
    let fixture = PendingFixture::new();
    let native = OwnedError::capture(QueryError::new(
        ErrorCode::WorkerError,
        anyhow::Error::new(fixture.failure.clone()).context(Primary),
    ));
    let mut r = Registry::new(instance());
    let h = running(&mut r, "a");
    r.apply(
        &h,
        RegistryUpdate::Joined(EngineCompletion {
            handle: h.clone(),
            result: Err(native),
        }),
    )
    .unwrap();
    let run = r.cached(&h).unwrap();
    assert_eq!(run.state, RunState::WorkerError);
    assert_eq!(run.cleanup.native, NativeCleanup::Pending);
    assert_eq!(run.cleanup.launch, Some(Launch::MayRun));
    assert!(run.cleanup.recovery_required);
    r.set_cleanup_blocked(true);
    r.set_cleanup_blocked(false);
    assert_eq!(
        r.admit(args("b"), true).unwrap_err().error.code,
        ErrorCode::CleanupPending
    );
    let before = frozen(&r, &h);
    let owners = r.take_error_owners_for_shutdown();
    assert_eq!(owners.len(), 1);
    assert!(owners[0].primary().is::<Primary>());
    assert_eq!(frozen(&r, &h), before);
    assert_eq!(
        r.admit(args("b"), true).unwrap_err().error.code,
        ErrorCode::CleanupPending
    );
    fixture.release_fault();
    drop(owners);
}
#[test]
fn terminal_error_is_not_replaced_by_snapshot_or_rejected_second_completion() {
    let mut r = Registry::new(instance());
    let h = running(&mut r, "a");
    r.apply(&h, RegistryUpdate::Bound(binding(&h, "run", 1)))
        .unwrap();
    r.apply(
        &h,
        RegistryUpdate::Joined(EngineCompletion {
            handle: h.clone(),
            result: Err(owner(ErrorCode::WorkerError)),
        }),
    )
    .unwrap();
    let error = serde_json::to_value(&r.cached(&h).unwrap().error).unwrap();
    r.apply(
        &h,
        RegistryUpdate::Refreshed(snapshot(RunOutcome::Passed, 1, GuestCleanup::Complete)),
    )
    .unwrap();
    assert_eq!(r.cached(&h).unwrap().state, RunState::WorkerError);
    assert!(r.cached(&h).unwrap().engine_result.is_none());
    assert_eq!(
        serde_json::to_value(&r.cached(&h).unwrap().error).unwrap(),
        error
    );
    assert!(r.apply(&h, finish(&h, RunOutcome::Passed)).is_err());
    assert_eq!(r.take_error_owners_for_shutdown().len(), 1);
}
#[test]
fn list_freezes_membership_but_reads_current_status_and_validates_every_cursor_coordinate() {
    let mut r = Registry::new(instance());
    assert!(
        r.list(&list_args(None, None), &reply())
            .unwrap()
            .runs
            .is_empty()
    );
    let h1 = reserve(&mut r, "a");
    r.cancel(&h1, CancellationReason::Client).unwrap();
    let h2 = reserve(&mut r, "b");
    let first = r.list(&list_args(Some(1), None), &reply()).unwrap();
    assert_eq!(first.high_water, 2);
    assert_eq!(first.runs[0].handle, h1);
    r.cancel(&h2, CancellationReason::Client).unwrap();
    reserve(&mut r, "c");
    let second = r
        .list(&list_args(Some(1), first.next_cursor.clone()), &reply())
        .unwrap();
    assert_eq!(second.high_water, 2);
    assert_eq!(second.runs[0].handle, h2);
    assert_eq!(second.runs[0].state, RunState::StartError);
    assert!(second.next_cursor.is_none());
    assert_eq!(
        r.list(&list_args(Some(2), first.next_cursor.clone()), &reply())
            .unwrap_err()
            .error
            .code,
        ErrorCode::InvalidCursor
    );
    let cursor = first.next_cursor.unwrap();
    let record = CursorRecord::decode(&cursor, &instance()).unwrap();
    for position in [2, 3] {
        let mut c = record.clone();
        c.next_position = position;
        if let Ok(cursor) = c.encode() {
            assert_eq!(
                r.list(&list_args(Some(1), Some(cursor)), &reply())
                    .unwrap_err()
                    .error
                    .code,
                ErrorCode::InvalidCursor
            );
        }
    }
    let mut c = record.clone();
    c.list_high_water = 4;
    let forged = c.encode().unwrap();
    assert_eq!(
        r.list(&list_args(Some(1), Some(forged)), &reply())
            .unwrap_err()
            .error
            .code,
        ErrorCode::InvalidCursor
    );
    let mut c = record.clone();
    c.instance = [2; 16];
    assert_eq!(
        r.list(&list_args(Some(1), Some(c.encode().unwrap())), &reply())
            .unwrap_err()
            .error
            .code,
        ErrorCode::StaleInstance
    );
    let mut c = record;
    c.selector_digest = [3; 16];
    assert_eq!(
        r.list(&list_args(Some(1), Some(c.encode().unwrap())), &reply())
            .unwrap_err()
            .error
            .code,
        ErrorCode::InvalidCursor
    );
    assert_eq!(
        r.list(&list_args(Some(0), None), &reply())
            .unwrap_err()
            .error
            .code,
        ErrorCode::InvalidArguments
    );
    assert_eq!(
        r.list(&list_args(Some(33), None), &reply())
            .unwrap_err()
            .error
            .code,
        ErrorCode::InvalidArguments
    );
}
#[test]
fn list_uses_complete_envelope_largest_prefix_and_keeps_requested_limit() {
    let mut r = Registry::new(instance());
    for n in 0..32 {
        let h = running(&mut r, &format!("k-{n}"));
        r.apply(&h, RegistryUpdate::Bound(binding(&h, "r", 1)))
            .unwrap();
        r.apply(
            &h,
            RegistryUpdate::Refreshed(snapshot(RunOutcome::Failed, 1, GuestCleanup::Unknown)),
        )
        .unwrap();
        let mut summary = FinalSummary {
            outcome: RunOutcome::Failed,
            counts: ResultCounts::default(),
            report_path: PathBuf::from("\"".repeat(512)),
            journal_path: PathBuf::from("\\".repeat(512)),
        };
        summary.counts.failed_checks = 42;
        r.apply(
            &h,
            RegistryUpdate::Joined(EngineCompletion {
                handle: h.clone(),
                result: Ok(summary),
            }),
        )
        .unwrap();
    }
    let page = r.list(&list_args(None, None), &reply()).unwrap();
    assert!(!page.runs.is_empty());
    assert!(page.runs.len() < 32);
    assert_eq!(page.high_water, 32);
    let cursor = CursorRecord::decode(page.next_cursor.as_ref().unwrap(), &instance()).unwrap();
    assert_eq!(cursor.requested_limit, 32);
    assert_eq!(cursor.next_position, page.runs.len() as u64);
    let encoded = encode(
        Era::Modern,
        &reply().id,
        &instance(),
        None,
        &Ok(ToolData::RunList(page.clone())),
    )
    .unwrap();
    assert!(encoded.bytes.len() <= 65_536);
    let full = r
        .list(&list_args(Some(32), page.next_cursor.clone()), &reply())
        .unwrap();
    let mut larger = page.clone();
    larger.runs.push(full.runs[0].clone());
    let mut next = cursor;
    next.next_position += 1;
    larger.next_cursor = Some(next.encode().unwrap());
    assert!(matches!(
        encode(
            Era::Modern,
            &reply().id,
            &instance(),
            None,
            &Ok(ToolData::RunList(larger))
        ),
        Err(EncodeFailure::Capacity(_))
    ));
}

#[test]
fn newly_rejected_conflict_has_no_handle_and_cleanup_precedes_full() {
    let mut r = Registry::new(instance());
    let first = reserve(&mut r, "a");
    let mut conflict = args("a");
    conflict.pull = true;
    let failure = r.admit(conflict, false).unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::IdempotencyConflict);
    assert!(failure.error.handle.is_none());
    r.cancel(&first, CancellationReason::Client).unwrap();
    for n in 2..=128 {
        let h = reserve(&mut r, &format!("key-{n}"));
        r.cancel(&h, CancellationReason::Client).unwrap();
    }
    r.set_cleanup_blocked(true);
    assert_eq!(
        r.admit(args("overflow"), false).unwrap_err().error.code,
        ErrorCode::CleanupPending
    );
    assert!(matches!(
        r.admit(args("a"), false),
        Ok(AdmissionDecision::Replay(_))
    ));
}

#[test]
fn every_rejected_owned_update_returns_original_primary_and_payload() {
    struct Witness(Arc<AtomicUsize>);
    impl Drop for Witness {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    for kind in 0..3 {
        let drops = Arc::new(AtomicUsize::new(0));
        let owner = OwnedError::capture(QueryError::new(
            ErrorCode::WorkerError,
            worker_panic(Box::new(Witness(drops.clone()))).context(Primary),
        ));
        let mut r = Registry::new(instance());
        let h = reserve(&mut r, "a");
        r.cancel(&h, CancellationReason::Client).unwrap();
        let update = match kind {
            0 => RegistryUpdate::Validation {
                generation: 1,
                result: Err(owner),
            },
            1 => RegistryUpdate::SpawnFailed(owner),
            _ => RegistryUpdate::Joined(EngineCompletion {
                handle: h.clone(),
                result: Err(owner),
            }),
        };
        let rejected = r.apply(&h, update).unwrap_err();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let owner = match rejected {
            RegistryUpdate::Validation {
                result: Err(owner), ..
            }
            | RegistryUpdate::SpawnFailed(owner) => owner,
            RegistryUpdate::Joined(EngineCompletion {
                result: Err(owner), ..
            }) => owner,
            _ => panic!("original owned variant changed"),
        };
        assert!(owner.primary().is::<Primary>());
        drop(owner);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(r.take_error_owners_for_shutdown().is_empty());
        assert_eq!(
            r.cached(&h).unwrap().error.as_ref().unwrap().code,
            ErrorCode::CancelledBeforeStart
        );
    }
}

#[test]
fn preview_cuts_native_display_at_utf8_boundary_without_mislabeling_literal_replacement() {
    let full = path_preview(Path::new(&"界".repeat(200)));
    assert_eq!(full.text, "界".repeat(170));
    assert_eq!(full.text.len(), 510);
    assert!(full.truncated);
    assert!(!full.lossy);
    let exact = path_preview(Path::new(&"a".repeat(512)));
    assert_eq!(exact.text.len(), 512);
    assert!(!exact.truncated);
    let literal = path_preview(Path::new("valid-�"));
    assert_eq!(literal.text, "valid-�");
    assert!(!literal.lossy);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let native = std::ffi::OsString::from_vec(vec![b'a', 0xff]);
        let preview = path_preview(Path::new(&native));
        assert_eq!(preview.text, "a�");
        assert!(preview.lossy);
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let native = std::ffi::OsString::from_wide(&[b'a' as u16, 0xd800]);
        let preview = path_preview(Path::new(&native));
        assert_eq!(preview.text, "a�");
        assert!(preview.lossy);
    }
}

#[test]
fn duplicate_generation_assignment_wrong_join_identity_and_future_sample_are_rejected() {
    let mut r = Registry::new(instance());
    let h = reserve(&mut r, "a");
    assert!(
        r.apply(
            &h,
            RegistryUpdate::Validation {
                generation: 1,
                result: Ok(recipe())
            }
        )
        .is_err()
    );
    start(&mut r, &h, 1);
    assert!(
        r.apply(&h, RegistryUpdate::ValidationStarted { generation: 2 })
            .is_err()
    );
    assert!(
        r.apply(
            &h,
            RegistryUpdate::ValidationExpired {
                generation: 2,
                code: ErrorCode::QueryTimeout
            }
        )
        .is_err()
    );
    r.apply(
        &h,
        RegistryUpdate::Validation {
            generation: 1,
            result: Ok(recipe()),
        },
    )
    .unwrap();
    r.take_dispatch(&h).unwrap();
    r.apply(&h, RegistryUpdate::Dispatched).unwrap();
    let wrong = Handle::parse(&format!("{}:2", instance().as_str())).unwrap();
    assert!(r.apply(&h, finish(&wrong, RunOutcome::Passed)).is_err());
    r.apply(&h, RegistryUpdate::Bound(binding(&h, "actual", 1)))
        .unwrap();
    let before = frozen(&r, &h);
    assert!(
        r.apply(
            &h,
            RegistryUpdate::Refreshed(snapshot(RunOutcome::Passed, 2, GuestCleanup::Complete))
        )
        .is_err()
    );
    assert_eq!(frozen(&r, &h), before);
    let mut overflow = FinalSummary {
        outcome: RunOutcome::Passed,
        counts: ResultCounts::default(),
        report_path: PathBuf::new(),
        journal_path: PathBuf::new(),
    };
    if let Ok(too_large) = usize::try_from(UINT_MAX + 1) {
        overflow.counts.commands = too_large;
        let rejected = r
            .apply(
                &h,
                RegistryUpdate::Joined(EngineCompletion {
                    handle: h.clone(),
                    result: Ok(overflow),
                }),
            )
            .unwrap_err();
        let RegistryUpdate::Joined(EngineCompletion {
            result: Ok(summary),
            ..
        }) = rejected
        else {
            panic!()
        };
        assert_eq!(summary.counts.commands, too_large);
        assert_eq!(frozen(&r, &h), before);
    }
}

#[test]
fn list_rejects_wrong_family_empty_continuation_and_explicit_stale_instances() {
    let r = Registry::new(instance());
    let a = list_args(Some(1), None);
    let cursor = CursorRecord::list(&a, 0).unwrap().encode().unwrap();
    assert_eq!(
        r.list(&list_args(Some(1), Some(cursor)), &reply())
            .unwrap_err()
            .error
            .code,
        ErrorCode::InvalidCursor
    );
    let mut stale = list_args(None, None);
    stale.server_instance = Instance::parse(&"2".repeat(32)).unwrap();
    assert_eq!(
        r.list(&stale, &reply()).unwrap_err().error.code,
        ErrorCode::StaleInstance
    );
    let mut r = Registry::new(instance());
    let h = reserve(&mut r, "a");
    r.cancel(&h, CancellationReason::Client).unwrap();
    reserve(&mut r, "b");
    let first = r.list(&a, &reply()).unwrap();
    let mut wrong = CursorRecord::decode(first.next_cursor.as_ref().unwrap(), &instance()).unwrap();
    wrong.family = super::super::cursor::Family::RecipeCatalog;
    wrong.list_high_water = 0;
    assert_eq!(
        r.list(&list_args(Some(1), Some(wrong.encode().unwrap())), &reply())
            .unwrap_err()
            .error
            .code,
        ErrorCode::InvalidCursor
    );
    assert_eq!(
        r.list(
            &list_args(None, Some(Cursor::parse("not-a-cursor").unwrap())),
            &reply()
        )
        .unwrap_err()
        .error
        .code,
        ErrorCode::InvalidCursor
    );
}

#[test]
fn joined_model_counts_remain_distinct_from_saved_snapshot_counts_without_binding() {
    let mut r = Registry::new(instance());
    let h = running(&mut r, "counts");
    r.apply(
        &h,
        RegistryUpdate::Joined(EngineCompletion {
            handle: h.clone(),
            result: Ok(FinalSummary {
                outcome: RunOutcome::Incomplete,
                counts: ResultCounts {
                    passed_checks: 1,
                    failed_checks: 2,
                    incomplete_checks: 3,
                    skipped_checks: 4,
                    review_findings: 5,
                    commands: 6,
                },
                report_path: PathBuf::from("actual-report.json"),
                journal_path: PathBuf::from("actual-journal.json"),
            }),
        }),
    )
    .unwrap();
    let run = r.cached(&h).unwrap();
    assert_eq!(run.state, RunState::Finished);
    assert_eq!(
        serde_json::to_value(&run.engine_result).unwrap(),
        json!({
            "outcome":"incomplete", "counts":{
                "passed_checks":1,"failed_checks":2,"incomplete_checks":3,
                "skipped_checks":4,"review_findings":5,"commands":6
            }
        })
    );
    assert!(run.saved.is_none());
    assert_eq!(run.cleanup.guest, GuestCleanup::Unknown);
    assert_eq!(run.cleanup.native, NativeCleanup::Complete);
    assert_eq!(run.cleanup.launch, None);
    assert_eq!(run.report_path.as_ref().unwrap().text, "actual-report.json");
    assert_eq!(
        run.journal_path.as_ref().unwrap().text,
        "actual-journal.json"
    );
    r.apply(&h, RegistryUpdate::Bound(binding(&h, "actual-run", 1)))
        .unwrap();
    r.apply(
        &h,
        RegistryUpdate::Refreshed(snapshot(RunOutcome::Passed, 1, GuestCleanup::Complete)),
    )
    .unwrap();
    assert_eq!(
        r.cached(&h)
            .unwrap()
            .saved
            .as_ref()
            .unwrap()
            .counts
            .commands
            .get(),
        0
    );
    assert_eq!(
        r.cached(&h)
            .unwrap()
            .engine_result
            .as_ref()
            .unwrap()
            .counts
            .commands
            .get(),
        6
    );
}
