use super::*;
use theseus_host::process::{
    HostCleanup, LaunchPhase,
    test_support::{self, LaunchFault},
};
#[derive(Default)]
struct Idle {
    cancelled: bool,
}
impl DuplexDriver for Idle {
    fn pending_write(&self) -> &[u8] {
        &[]
    }
    fn written(&mut self, _: usize) -> Result<()> {
        Ok(())
    }
    fn receive(&mut self, _: &[u8]) {}
    fn eof(&mut self) {}
    fn cancel(&mut self) {
        self.cancelled = true;
    }
    fn wants_close(&self) -> bool {
        false
    }
}
#[test]
fn actual_native_failed_launch_preserves_phase_owner_and_no_execution_before_resume() {
    for (fault, expected, pending) in [
        (LaunchFault::BeforeCreate, LaunchPhase::NeverCreated, false),
        (
            LaunchFault::CreatedSuspended,
            LaunchPhase::CreatedSuspended,
            false,
        ),
        (LaunchFault::ResumeUncertain, LaunchPhase::MayRun, false),
        (
            LaunchFault::CleanupPending,
            LaunchPhase::CreatedSuspended,
            true,
        ),
        (
            LaunchFault::BeforeCreatePending,
            LaunchPhase::NeverCreated,
            true,
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("ran");
        let mut command = super::super::test_support::fixture("launch-marker", &[]);
        command.arg(&marker);
        let mut aborted = 0;
        let cancel = CancellationToken::default();
        let cleanup_deadline = Instant::now() + Duration::from_secs(3);
        let output = run_duplex_with(
            &mut command,
            Duration::from_secs(3),
            &cancel,
            None,
            32,
            32,
            Idle::default(),
            || {
                aborted += 1;
                cancel.cancel();
                Ok(())
            },
            |command| {
                test_support::spawn(command, InputMode::Pipe, fault)
                    .map(|child| HostProcess::acquired(child, Instant::now()))
                    .map_err(|mut failure| {
                        if pending {
                            let HostCleanup::Pending(owner) = &mut failure.cleanup else {
                                panic!("native pending fault lost actual cleanup owner")
                            };
                            test_support::fail_cleanup_attempts(owner, 4);
                        }
                        failure
                    })
            },
        )
        .unwrap();
        assert_eq!(aborted, 1, "guest preparation always requires owned abort");
        assert!(
            output.process.cancelled && output.driver.cancelled,
            "native failed-acquisition abort cancellation reaches both output and driver"
        );
        assert_eq!(output.acquisition_started, expected == LaunchPhase::MayRun);
        let failure = output.launch_failure.unwrap();
        assert_eq!(failure.launch(), expected);
        if expected != LaunchPhase::MayRun {
            assert!(!marker.exists());
        }
        eprintln!(
            "native failure phase={expected:?} injected_pending={pending} initial_pending={}",
            failure.pending()
        );
        if pending {
            assert!(
                failure.pending(),
                "injected pending fault must retain actual authority"
            );
            assert!(failure.retry_cleanup().is_err());
            assert!(failure.pending());
            assert!(!failure.retry_cleanup().unwrap());
            assert!(failure.pending());
            let mut retained = CleanupFailures::default();
            retained.retain(&anyhow::Error::new(failure.clone()));
            retained.retain(&anyhow::Error::new(failure.clone()));
            let error = retained
                .finish()
                .expect_err("injected native cleanup still pending");
            assert!(error.downcast_ref::<CleanupFailures>().is_some());
            super::super::release_test_fault(&failure);
        }
        // Ordinary eager cleanup can still be Pending on return: termination
        // requests are asynchronous. Observe real completion using one clock.
        while !failure.retry_cleanup().unwrap() {
            assert!(Instant::now() < cleanup_deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(!failure.pending());
        if expected != LaunchPhase::MayRun {
            assert!(
                !marker.exists(),
                "pre-resume fixture must never execute, including after cleanup"
            );
        }
    }
}
