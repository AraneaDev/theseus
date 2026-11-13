use super::*;
pub(super) fn complete() -> Completion {
    Completion {
        panic: None,
        cleanup: CleanupFailures::default(),
        status: None,
        timed_out: false,
        cancelled: false,
        supervised_stop: false,
        error: None,
        duration_ms: 0,
    }
}
#[test]
fn actual_joined_parser_panic_precedes_stderr_error_after_cleanup() {
    let parser = thread::spawn(|| -> Result<Snapshot> { panic!("original parser panic") });
    let stderr = thread::spawn(|| -> Result<Drain> { Err(anyhow!("stderr write failed")) });
    let parsed = parser.join();
    let stderr = stderr.join();
    assert!(parsed.is_err() && stderr.as_ref().is_ok_and(Result::is_err));
    let error = finish_archive_workers(complete(), parsed, stderr).unwrap_err();
    assert!(
        format!("{error:#}").contains("archive parser panicked"),
        "{error:#}"
    );
}
#[test]
fn actual_joined_parser_rejection_keeps_existing_stderr_and_timeout_precedence() {
    for failing_stderr in [true, false] {
        let parser =
            thread::spawn(|| -> Result<Snapshot> { Err(anyhow!("parser rejected archive")) });
        let stderr = thread::spawn(move || -> Result<Drain> {
            if failing_stderr {
                Err(anyhow!("stderr write failed"))
            } else {
                Ok(Drain::empty())
            }
        });
        let mut completion = complete();
        completion.timed_out = true;
        let error = finish_archive_workers(completion, parser.join(), stderr.join()).unwrap_err();
        assert!(format!("{error:#}").contains(if failing_stderr {
            "stderr write failed"
        } else {
            "timed out or cancelled"
        }));
    }
}
