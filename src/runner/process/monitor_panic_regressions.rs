use super::*;
#[test]
fn supervisor_panic_still_stops_io_aborts_guest_and_finishes_owned_child() {
    for panic_on_poll in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let mut command = test_support::fixture("ready-hold", &[]);
        command.args([temp.path().join("ready"), temp.path().join("heartbeat")]);
        command.arg("10000");
        let mut process = HostProcess::spawn(&command).unwrap();
        let stop = process.stop.clone();
        let cancel = CancellationToken::default();
        if !panic_on_poll {
            cancel.cancel();
        }
        let mut aborted = false;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            process.monitor(
                Duration::from_secs(3),
                &cancel,
                || false,
                || false,
                MonitorPolicy::Immediate,
                |event| match event {
                    SupervisionEvent::Poll => panic!("original supervisor panic"),
                    SupervisionEvent::Abort => {
                        assert!(stop.load(Ordering::SeqCst), "stop IO precedes guest abort");
                        aborted = true;
                        if !panic_on_poll {
                            panic!("original abort panic");
                        }
                        Ok(SupervisionDecision::Continue)
                    }
                },
            )
        }));
        let released = process.child.is_none();
        // Rescue the RED case before asserting.
        drop(process);
        assert!(
            result.is_ok(),
            "monitor unwound before caller could join workers"
        );
        assert!(aborted, "supervisor panic skipped prepared guest abort");
        assert!(released, "monitor panic skipped owned native child cleanup");
    }
}
