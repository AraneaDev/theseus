use super::*;
#[test]
fn cached_report_cannot_hide_live_native_cleanup_owner() {
    use crate::runner::process::native_tests::{finish_native, suspended_failure};
    let (temp, failure) = suspended_failure();
    let mut app = App::offline(
        crate::model::RunReport::new("cached"),
        temp.path().to_path_buf(),
    );
    app.active = true;
    let error = complete_run(&mut app, Err(failure.clone().into()))
        .expect_err("cached evidence must not discard native cleanup authority");
    let mut retained = crate::runner::process::CleanupFailures::default();
    retained.retain(&error);
    assert!(retained.pending());
    assert!(!app.active);
    finish_native(&temp, &failure);
    retained.finish().unwrap();
}
#[test]
fn restored_runtime_return_preserves_worker_panic_owner_over_original_ui_error() {
    use crate::runner::process::{
        CleanupFailures,
        native_tests::{finish_native, suspended_failure},
    };
    let (temp, failure) = suspended_failure();
    let mut cleanup = CleanupFailures::default();
    cleanup.retain(&failure.clone().into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (tx, rx) = oneshot::channel::<Result<()>>();
    runtime.spawn_blocking(move || {
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cleanup.resume_unwind(Box::new("original native worker panic"))
        }))
        .unwrap_err();
        tx.send(Err(crate::runner::process::worker_panic(panic)))
            .unwrap();
    });
    let restored = std::cell::Cell::new(false);
    let error = finish_runtime(
        runtime,
        &CancellationToken::default(),
        Some(rx),
        Err(anyhow!("original UI timeout")),
        || {
            restored.set(true);
            Err(anyhow!("native mode restoration failed"))
        },
    )
    .unwrap_err();
    assert!(restored.get());
    assert!(error.to_string().contains("original UI timeout"));
    assert!(error.to_string().contains("native mode restoration failed"));
    let mut retained = CleanupFailures::default();
    retained.retain(&error);
    assert!(retained.pending());
    finish_native(&temp, &failure);
    retained.finish().unwrap();
}
