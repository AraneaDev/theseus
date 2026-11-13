use super::*;
#[derive(Default)]
struct Reply {
    received: bool,
}
impl DuplexDriver for Reply {
    fn pending_write(&self) -> &[u8] {
        &[]
    }
    fn written(&mut self, _: usize) -> Result<()> {
        Ok(())
    }
    fn receive(&mut self, _: &[u8]) {
        self.received = true;
    }
    fn eof(&mut self) {}
    fn cancel(&mut self) {}
    fn wants_close(&self) -> bool {
        self.received
    }
}
#[test]
fn early_interactive_shutdown_keeps_timeout_decision_before_emergency_cleanup() {
    for cancel_during_cleanup in [false, true] {
        let cancel = CancellationToken::default();
        let began = Instant::now();
        let mut command = super::super::test_support::fixture("duplex-case", &["reply-hold"]);
        let mut aborted = false;
        let out = run_duplex(
            &mut command,
            Duration::from_secs(2),
            &cancel,
            None,
            128,
            128,
            Reply::default(),
            || {
                assert!(
                    began.elapsed() < Duration::from_secs(2),
                    "shutdown decision precedes execution deadline"
                );
                aborted = true;
                thread::sleep(Duration::from_millis(1200));
                if cancel_during_cleanup {
                    cancel.cancel();
                }
                Ok(())
            },
        )
        .unwrap();
        assert!(aborted && out.forced_abort);
        assert!(began.elapsed() > Duration::from_secs(2));
        assert!(
            !out.process.timed_out,
            "separate cleanup elapsed must not fabricate execution timeout"
        );
        assert_eq!(out.process.cancelled, cancel_during_cleanup);
        assert_eq!(out.process.status, None);
    }
}
#[test]
fn pre_spawn_cancellation_and_spawn_error_retain_explicit_not_started_driver() {
    for cancelled in [true, false] {
        let cancel = CancellationToken::default();
        if cancelled {
            cancel.cancel();
        }
        let mut aborted = false;
        let out = run_duplex(
            &mut Command::new("/missing-duplex-executable"),
            Duration::from_secs(3),
            &cancel,
            None,
            32,
            32,
            Reply::default(),
            || {
                aborted = true;
                Ok(())
            },
        )
        .unwrap();
        assert!(!out.acquisition_started);
        assert!(aborted && out.forced_abort);
        assert!(!out.driver.received);
        assert_eq!(out.process.cancelled, cancelled);
        assert_eq!(out.process.status, None);
        assert_eq!(out.process.error.is_some(), !cancelled);
    }
}
