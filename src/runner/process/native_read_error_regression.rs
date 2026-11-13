use super::*;
#[test]
fn reader_error_settles_actual_pending_endpoint_before_returning_original_error() {
    let command = test_support::fixture("hold-pipes", &["30000"]);
    let mut process = HostProcess::spawn(&command).unwrap();
    let mut reader = PipeReader::new(process.stdout.take().unwrap(), process.stop.clone()).unwrap();
    let pending = reader
        .reader
        .as_mut()
        .unwrap()
        .try_read(&mut [0; 1])
        .unwrap_err();
    assert_eq!(pending.kind(), std::io::ErrorKind::WouldBlock);
    // Inject the failure at the application's read-error boundary. The pipe
    // and, on Windows, its outstanding operation are real native owners.
    let error = reader.read_failed(std::io::Error::other("original read observation failure"));
    let state = reader
        .reader
        .as_mut()
        .unwrap()
        .try_read(&mut [0; 1])
        .unwrap_err()
        .kind();
    drop(reader);
    drop(process);
    assert_eq!(error.to_string(), "original read observation failure");
    assert_eq!(
        state,
        std::io::ErrorKind::Interrupted,
        "ordinary error return left native IO active instead of observing cancellation/completion"
    );
}
