use super::*;
#[test]
fn settled_errors_keep_ui_then_worker_then_restoration_precedence() {
    for (ui_fails, worker_fails, expected) in [
        (true, true, "UI"),
        (false, true, "worker"),
        (false, false, "restoration"),
    ] {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (tx, rx) = oneshot::channel::<Result<()>>();
        tx.send(if worker_fails {
            Err(std::io::Error::other("worker").into())
        } else {
            Ok(())
        })
        .unwrap();
        let error = finish_runtime(
            runtime,
            &CancellationToken::default(),
            Some(rx),
            if ui_fails {
                Err(std::io::Error::other("UI").into())
            } else {
                Ok(0)
            },
            || Err(std::io::Error::other("restoration").into()),
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().to_string(),
            expected
        );
    }
}
#[test]
fn pending_cleanup_preserves_original_ui_error_and_every_owner() {
    pending_primary_error("UI");
}
#[test]
fn pending_cleanup_preserves_primary_worker_error_and_every_owner() {
    pending_primary_error("worker");
}
#[test]
fn pending_cleanup_preserves_primary_restoration_error_and_every_owner() {
    pending_primary_error("terminal restoration");
}
fn pending_primary_error(primary: &'static str) {
    use crate::runner::process::{CleanupFailures, test_support::PendingFixture, worker_panic};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    #[derive(Debug)]
    struct Original {
        stage: &'static str,
        source: anyhow::Error,
    }
    impl std::fmt::Display for Original {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "original {} failure", self.stage)
        }
    }
    impl std::error::Error for Original {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(self.source.as_ref())
        }
    }
    struct Payload(Arc<AtomicBool>);
    impl Drop for Payload {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let first = PendingFixture::new();
    let second = PendingFixture::new();
    let payload_dropped = Arc::new(AtomicBool::new(false));
    let mut cleanup = CleanupFailures::default();
    cleanup.retain(&second.failure.clone().into());
    if primary == "terminal restoration" {
        cleanup.retain(&first.failure.clone().into());
    }
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cleanup.resume_unwind(Box::new(Payload(payload_dropped.clone())));
    }))
    .unwrap_err();
    let panic = worker_panic(panic);
    let (restoration, worker) = if primary == "terminal restoration" {
        (panic, Ok(()))
    } else {
        (
            first.failure.clone().into(),
            Err(Original {
                stage: "worker",
                source: panic,
            }
            .into()),
        )
    };
    let ui = if primary == "UI" {
        Err(Original {
            stage: "UI",
            source: std::io::Error::other("original UI source").into(),
        }
        .into())
    } else {
        Ok(0)
    };
    let (restore_tx, restore_rx) = mpsc::channel();
    let (deliver_tx, deliver_rx) = mpsc::channel();
    let (delivered_tx, delivered_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let (returned_tx, returned_rx) = mpsc::channel();
    let owner = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (tx, rx) = oneshot::channel::<Result<()>>();
        runtime.spawn_blocking(move || {
            deliver_rx.recv().unwrap();
            tx.send(worker).unwrap();
            delivered_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
        });
        let result = finish_runtime(runtime, &CancellationToken::default(), Some(rx), ui, || {
            restore_tx.send(()).unwrap();
            Err(Original {
                stage: "terminal restoration",
                source: restoration,
            }
            .into())
        });
        returned_tx.send(result).unwrap();
    });
    restore_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let before_delivery = returned_rx.recv_timeout(Duration::from_millis(50));
    deliver_tx.send(()).unwrap();
    delivered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let before_join = returned_rx.recv_timeout(Duration::from_millis(50));
    finish_tx.send(()).unwrap();
    owner.join().unwrap();
    let error = returned_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap_err();
    let typed_primary = error.downcast_ref::<Original>().map(|error| error.stage);
    let original_in_chain = error.chain().any(|source| {
        source
            .downcast_ref::<Original>()
            .is_some_and(|e| e.stage == primary)
    });
    let source_retained = error.downcast_ref::<Original>().is_some_and(|original| {
        std::error::Error::source(original).is_some_and(|source| {
            if primary == "UI" {
                source.downcast_ref::<std::io::Error>().is_some()
                    && source.to_string() == "original UI source"
            } else {
                source.to_string().contains("engine worker panicked")
            }
        })
    });
    let diagnostics = format!("{error:#}");
    let mut retained = CleanupFailures::default();
    retained.retain(&error);
    drop(error);
    let payload_retained = !payload_dropped.load(Ordering::SeqCst);
    let recollected = retained.pending();
    first.release_fault();
    let pending = retained.finish().err();
    let first_retried = !first.failure.pending();
    let second_pending = second.failure.pending() && pending.is_some();
    let mut retained = CleanupFailures::default();
    if let Some(error) = &pending {
        retained.retain(error);
    }
    drop(pending);
    let payload_recollected = !payload_dropped.load(Ordering::SeqCst);
    second.release_fault();
    let settled = retained.finish().is_ok();
    let second_retried = !second.failure.pending();
    // Rescue both real native children before any regression assertion on RED.
    drop(first);
    drop(second);
    assert!(
        before_delivery.is_err(),
        "receiver completion was not awaited"
    );
    assert!(
        before_join.is_err(),
        "delivered result detached its blocking worker"
    );
    assert_eq!(typed_primary, Some(primary), "original typed error lost");
    assert!(
        original_in_chain && source_retained,
        "original source chain lost"
    );
    assert!(
        diagnostics.contains("original terminal restoration failure"),
        "{diagnostics}"
    );
    assert!(
        diagnostics.contains("owned fixture cleanup witness"),
        "{diagnostics}"
    );
    assert!(
        diagnostics.contains("non-string panic payload retained"),
        "{diagnostics}"
    );
    if primary != "terminal restoration" {
        assert!(
            diagnostics.contains("original worker failure"),
            "{diagnostics}"
        );
    }
    if primary == "UI" {
        assert!(diagnostics.contains("original UI source"), "{diagnostics}");
    }
    assert!(
        recollected && first_retried && second_pending && settled && second_retried,
        "fresh collectors must retry every real owner after each fault release"
    );
    assert!(
        payload_retained && payload_recollected,
        "original panic payload lost"
    );
    assert!(
        payload_dropped.load(Ordering::SeqCst),
        "settled payload leaked"
    );
}
#[test]
fn terminal_restoration_precedes_join_and_delivery_does_not_detach_blocking_worker() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    let restored = Arc::new(AtomicBool::new(false));
    let observed = restored.clone();
    let (release_tx, release_rx) = mpsc::channel();
    let (worker_done_tx, worker_done_rx) = mpsc::channel();
    let (returned_tx, returned_rx) = mpsc::channel();
    let owner = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (result_tx, result_rx) = oneshot::channel();
        let (delivered_tx, delivered_rx) = mpsc::channel();
        runtime.spawn_blocking(move || {
            result_tx.send(Ok(())).unwrap();
            delivered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            worker_done_tx.send(()).unwrap();
        });
        delivered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let result = finish_runtime(
            runtime,
            &CancellationToken::default(),
            Some(result_rx),
            Err(anyhow!("original UI cleanup deadline")),
            || {
                observed.store(true, Ordering::SeqCst);
                Ok(())
            },
        );
        returned_tx.send(result).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while !restored.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let early = returned_rx.recv_timeout(Duration::from_millis(50));
    // Rescue the controlled peer before assertions, even for the RED run.
    release_tx.send(()).unwrap();
    worker_done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    owner.join().unwrap();
    assert!(
        early.is_err(),
        "ordinary return detached a still-running blocking worker"
    );
    let result = returned_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("original UI cleanup deadline"),
        "late worker success must not erase original UI timeout"
    );
}
#[test]
fn restore_failure_joins_worker_and_keeps_original_error_with_all_diagnostics() {
    use std::sync::mpsc;
    #[derive(Debug)]
    struct Original;
    impl std::fmt::Display for Original {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "original UI failure")
        }
    }
    impl std::error::Error for Original {}
    let (release_tx, release_rx) = mpsc::channel();
    let (restored_tx, restored_rx) = mpsc::channel();
    let (returned_tx, returned_rx) = mpsc::channel();
    let owner = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (tx, rx) = oneshot::channel::<Result<()>>();
        runtime.spawn_blocking(move || {
            release_rx.recv().unwrap();
            tx.send(Err(anyhow!("late worker failure"))).unwrap();
        });
        let result = finish_runtime(
            runtime,
            &CancellationToken::default(),
            Some(rx),
            Err(Original.into()),
            || {
                restored_tx.send(()).unwrap();
                Err(anyhow!("native mode restore failure"))
            },
        );
        returned_tx.send(result).unwrap();
    });
    restored_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let early = returned_rx.recv_timeout(Duration::from_millis(50));
    release_tx.send(()).unwrap();
    owner.join().unwrap();
    assert!(early.is_err(), "restoration error must not detach worker");
    let error = returned_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap_err();
    assert!(
        error.downcast_ref::<Original>().is_some(),
        "original typed error lost"
    );
    let diagnostic = format!("{error:#}");
    assert!(
        diagnostic.contains("native mode restore failure"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("late worker failure"), "{diagnostic}");
}
#[test]
fn restore_failure_cannot_be_successful_exit() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let result = finish_runtime::<()>(runtime, &CancellationToken::default(), None, Ok(0), || {
        Err(anyhow!("restore failed"))
    });
    assert!(
        result.is_err(),
        "failed terminal restoration became success"
    );
}
#[test]
fn disconnected_worker_result_restores_terminal_then_returns_error() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (tx, rx) = oneshot::channel::<Result<()>>();
    runtime.spawn_blocking(move || drop(tx));
    let restored = std::cell::Cell::new(false);
    let error = finish_runtime(
        runtime,
        &CancellationToken::default(),
        Some(rx),
        Ok(0),
        || {
            restored.set(true);
            Ok(())
        },
    )
    .unwrap_err();
    assert!(restored.get());
    assert!(error.to_string().contains("without delivering"));
}
