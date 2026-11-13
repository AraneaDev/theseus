use super::*;
#[test]
fn stopped_empty_worker_joins_without_native_owners() {
    let (dispatcher, ports, _, tokens) = channels();
    dispatcher
        .stop
        .store(true, std::sync::atomic::Ordering::Release);
    let join = spawn(ports, tokens).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !join.is_finished() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(matches!(
        join.join().unwrap(),
        WorkerCompletion::Clean {
            native_settled: true,
            ..
        }
    ));
}
