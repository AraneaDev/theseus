use super::*;
use std::cell::Cell;
#[test]
fn expired_exchange_still_starts_independent_cleanup_clock_without_retry_reset() {
    let command = super::super::test_support::fixture("exit", &["0"]);
    let mut process = HostProcess::spawn(&command).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while !process.exited_without_reaping().unwrap() {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    let now = Cell::new(process.started + Duration::from_secs(9));
    let expected = now.get() + Duration::from_secs(3);
    let completion = process.monitor_with_clock(
        Duration::from_secs(2),
        &CancellationToken::default(),
        || false,
        || false,
        MonitorPolicy::Duplex(&Signals::default()),
        |event| {
            assert!(matches!(event, SupervisionEvent::Abort));
            now.set(now.get() + Duration::from_secs(10));
            Ok(SupervisionDecision::Continue)
        },
        || now.get(),
    );
    assert!(completion.timed_out);
    assert_eq!(process.cleanup_deadline, Some(expected));
    assert_eq!(
        process.begin_cleanup_at(now.get()),
        expected,
        "retry must not mint another cleanup budget"
    );
}
#[test]
fn cancellation_during_cleanup_does_not_reclassify_prior_grace_decision_as_timeout() {
    let command = super::super::test_support::fixture("exit", &["0"]);
    let mut process = HostProcess::spawn(&command).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while !process.exited_without_reaping().unwrap() {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    let cancel = CancellationToken::default();
    let now = Cell::new(process.started + Duration::from_millis(100));
    let signals = Signals::default();
    signals.close.store(true, Ordering::SeqCst);
    let completion = process.monitor_with_clock(
        Duration::from_secs(5),
        &cancel,
        || {
            now.set(now.get() + Duration::from_millis(1100));
            false
        },
        || false,
        MonitorPolicy::Duplex(&signals),
        |event| {
            assert!(matches!(event, SupervisionEvent::Abort));
            now.set(now.get() + Duration::from_secs(20));
            cancel.cancel();
            Ok(SupervisionDecision::Continue)
        },
        || now.get(),
    );
    assert!(completion.cancelled);
    assert!(
        !completion.timed_out,
        "time spent in separately bounded cleanup is not execution time"
    );
    assert_eq!(
        process.cleanup_deadline,
        Some(process.started + Duration::from_millis(4200))
    );
}
