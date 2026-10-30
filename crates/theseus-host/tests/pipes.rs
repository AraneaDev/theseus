//! Real owned peers; these bounds assume a healthy kernel that completes cancellation.
use std::io::{self, ErrorKind};
use std::time::{Duration, Instant};
use theseus_host::process::{
    AcquiredChild, ChildEnvironment, InputMode, NativeCommand, OwnedChild, PipeReader,
};

fn spawn(args: &[&str], input: InputMode) -> AcquiredChild {
    OwnedChild::spawn(
        &NativeCommand {
            program: env!("CARGO_BIN_EXE_theseus-host-fixture").into(),
            args: args.iter().map(Into::into).collect(),
            cwd: None,
            environment: ChildEnvironment::Inherit,
        },
        input,
    )
    .unwrap()
}
fn tick(until: Instant) {
    assert!(
        Instant::now() < until,
        "healthy local peer must make bounded progress"
    );
    std::thread::sleep(Duration::from_millis(1));
}
fn drain(reader: &mut PipeReader, size: usize) -> io::Result<Vec<u8>> {
    let until = Instant::now() + Duration::from_secs(10);
    let mut result = Vec::new();
    let mut bytes = vec![0; size];
    loop {
        match reader.try_read(&mut bytes) {
            Ok(0) => return Ok(result),
            Ok(n) => result.extend_from_slice(&bytes[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => tick(until),
            Err(e) => return Err(e),
        }
    }
}
fn wait(child: &mut OwnedChild) {
    let until = Instant::now() + Duration::from_secs(10);
    while !child.exited_reserved().unwrap() {
        tick(until);
    }
}
#[test]
fn empty_peer_eof_and_buffered_split_reads() -> io::Result<()> {
    for (hex, expected) in [
        ("", &b""[..]),
        (
            "00010203040506070809",
            &b"\0\x01\x02\x03\x04\x05\x06\x07\x08\x09"[..],
        ),
    ] {
        let mut acquired = spawn(&["echo-bytes", hex], InputMode::Null);
        assert_eq!(drain(&mut acquired.stdout, 3)?, expected);
        assert_eq!(drain(&mut acquired.stderr, 2)?, b"");
        wait(&mut acquired.child);
        assert!(acquired.child.reap_and_release()?.success());
    }
    Ok(())
}
#[test]
fn both_streams_progress_under_backpressure() -> io::Result<()> {
    let mut acquired = spawn(&["flood-duplex", "262144"], InputMode::Null);
    let until = Instant::now() + Duration::from_secs(10);
    let mut counts = [0usize; 2];
    let mut eof = [false; 2];
    while !eof.iter().all(|x| *x) {
        for (i, reader) in [&mut acquired.stdout, &mut acquired.stderr]
            .into_iter()
            .enumerate()
        {
            if eof[i] {
                continue;
            }
            let mut bytes = [0; 997];
            match reader.try_read(&mut bytes) {
                Ok(0) => eof[i] = true,
                Ok(n) => {
                    assert!(bytes[..n].iter().all(|b| *b == b"OE"[i]));
                    counts[i] += n;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
        }
        tick(until);
    }
    assert_eq!(counts, [262144; 2]);
    wait(&mut acquired.child);
    assert!(acquired.child.reap_and_release()?.success());
    Ok(())
}
#[test]
fn partial_writes_close_stdin_delivers_eof_while_outputs_continue() -> io::Result<()> {
    let mut acquired = spawn(&["read-until-eof", "262144"], InputMode::Pipe);
    let writer = acquired.stdin.as_mut().unwrap();
    let input: Vec<_> = (0..262144).map(|n| (n % 251) as u8).collect();
    let until = Instant::now() + Duration::from_secs(10);
    let mut written = 0;
    let mut partial = false;
    while written < input.len() {
        match writer.try_write(&input[written..]) {
            Ok(n) => {
                assert!(n > 0);
                partial |= n < input.len() - written;
                written += n;
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => tick(until),
            Err(e) => return Err(e),
        }
    }
    assert!(partial, "bounded writes must acknowledge partial progress");
    // Deferred M1: observe actual stdout silence before EOF, not only child liveness.
    let quiet_until = Instant::now() + Duration::from_millis(100);
    while Instant::now() < quiet_until {
        assert_eq!(
            acquired.stdout.try_read(&mut [0; 1]).unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
        assert!(!acquired.child.exited_reserved()?);
        std::thread::sleep(Duration::from_millis(1));
    }
    writer.close()?;
    writer.close()?;
    assert!(writer.try_write(b"late").is_err());
    assert_eq!(drain(&mut acquired.stdout, 331)?, input);
    wait(&mut acquired.child);
    assert!(acquired.child.reap_and_release()?.success());
    Ok(())
}
#[test]
fn full_stdin_close_cancels_pending_write_without_waiting_for_peer() -> io::Result<()> {
    let mut acquired = spawn(&["ignore-stdin", "30000"], InputMode::Pipe);
    let writer = acquired.stdin.as_mut().unwrap();
    let bytes = vec![b'x'; 65536];
    let until = Instant::now() + Duration::from_secs(10);
    let mut accepted = 0;
    let mut blocked_since = Instant::now();
    loop {
        match writer.try_write(&bytes) {
            Ok(n) => {
                accepted += n;
                assert!(accepted < 4 * 1024 * 1024);
                blocked_since = Instant::now();
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if blocked_since.elapsed() >= Duration::from_millis(100) {
                    break;
                }
            }
            Err(e) => return Err(e),
        }
        tick(until);
    }
    let start = Instant::now();
    writer.close()?;
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(
        acquired.stdout.try_read(&mut [0; 1]).unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
    acquired.stdout.cancel_and_complete()?;
    assert_eq!(
        acquired.stdout.try_read(&mut [0; 1]).unwrap_err().kind(),
        ErrorKind::Interrupted
    );
    acquired.child.terminate_owned()?;
    acquired.child.reap_and_release()?;
    Ok(())
}
#[test]
fn inherited_pipe_stays_open_after_leader_exit_and_cancel_is_not_eof() -> io::Result<()> {
    let mut acquired = spawn(&["descendant", "30000"], InputMode::Null);
    wait(&mut acquired.child);
    let until = Instant::now() + Duration::from_secs(10);
    let mut marker = Vec::new();
    while !marker.ends_with(b"\n") {
        let mut bytes = [0; 64];
        match acquired.stdout.try_read(&mut bytes) {
            Ok(0) => panic!("descendant must retain stdout"),
            Ok(n) => marker.extend_from_slice(&bytes[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => tick(until),
            Err(e) => return Err(e),
        }
    }
    assert_eq!(
        acquired.stdout.try_read(&mut [0; 1]).unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
    acquired.stdout.cancel_and_complete()?;
    assert_eq!(
        acquired.stdout.try_read(&mut [0; 1]).unwrap_err().kind(),
        ErrorKind::Interrupted
    );
    acquired.child.terminate_owned()?;
    acquired.stderr.cancel_and_complete()?;
    acquired.child.reap_and_release()?;
    Ok(())
}
#[test]
fn cancellation_races_real_completion_and_scoped_panic_joins() -> io::Result<()> {
    for _ in 0..32 {
        let mut acquired = spawn(&["echo-bytes", "01020304"], InputMode::Null);
        let _ = acquired.stdout.try_read(&mut [0; 1]);
        acquired.stdout.cancel_and_complete()?;
        assert_eq!(
            acquired.stdout.try_read(&mut [0; 1]).unwrap_err().kind(),
            ErrorKind::Interrupted
        );
        acquired.child.terminate_owned()?;
        acquired.child.reap_and_release()?;
    }
    let mut acquired = spawn(&["hold-pipes", "30000"], InputMode::Pipe);
    let start = Instant::now();
    let result = std::thread::scope(|scope| {
        let mut stdout = acquired.stdout;
        scope
            .spawn(move || {
                assert_eq!(
                    stdout.try_read(&mut [0; 1]).unwrap_err().kind(),
                    ErrorKind::WouldBlock
                );
                // Unwinding drops a genuinely pending read; no manual cancellation.
                panic!("original scoped worker failure");
            })
            .join()
    });
    assert_eq!(
        *result.unwrap_err().downcast::<&str>().unwrap(),
        "original scoped worker failure"
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    acquired.stdin.as_mut().unwrap().close()?;
    acquired.stderr.cancel_and_complete()?;
    acquired.child.terminate_owned()?;
    acquired.child.reap_and_release()?;
    Ok(())
}

#[test]
fn failure_after_endpoint_setup_preserves_never_created_diagnostics() {
    let command = NativeCommand {
        program: std::env::temp_dir()
            .join("theseus-guaranteed-missing-parent")
            .join("missing-fixture-executable")
            .into(),
        args: Vec::new(),
        cwd: None,
        environment: ChildEnvironment::Inherit,
    };
    for input in [InputMode::Null, InputMode::Pipe] {
        let failure = OwnedChild::spawn(&command, input).unwrap_err();
        assert_eq!(
            failure.launch,
            theseus_host::process::LaunchPhase::NeverCreated
        );
        assert!(matches!(
            failure.cleanup,
            theseus_host::process::HostCleanup::Complete
        ));
        assert!(failure.cleanup_errors.is_empty());
        assert_eq!(failure.error.kind(), ErrorKind::NotFound);
    }
}

#[test]
fn cancelled_full_stdin_reaches_child_eof_and_output_continues() -> io::Result<()> {
    let mut acquired = spawn(
        &["delayed-read-until-eof", "1048576", "500"],
        InputMode::Pipe,
    );
    let writer = acquired.stdin.as_mut().unwrap();
    let bytes = vec![b'x'; 65536];
    let until = Instant::now() + Duration::from_secs(10);
    let mut accepted = 0;
    let mut blocked_since = Instant::now();
    loop {
        match writer.try_write(&bytes) {
            Ok(n) => {
                accepted += n;
                assert!(accepted < 1048576);
                blocked_since = Instant::now();
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if blocked_since.elapsed() >= Duration::from_millis(100) {
                    break;
                }
            }
            Err(e) => return Err(e),
        }
        tick(until);
    }
    assert_eq!(
        acquired.stdout.try_read(&mut [0; 1]).unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
    let closing = Instant::now();
    writer.close()?;
    assert!(closing.elapsed() < Duration::from_secs(2));
    let output = drain(&mut acquired.stdout, 511)?;
    // CancelIoEx may race a partially delivered write; acknowledged bytes cannot
    // disappear, and the one unacknowledged bounded prefix is the only allowance.
    assert!(output.len() >= accepted);
    assert!(output.len() <= accepted + 16384);
    assert!(output.iter().all(|byte| *byte == b'x'));
    assert_eq!(drain(&mut acquired.stderr, 16)?, b"stdin EOF received\n");
    wait(&mut acquired.child);
    assert!(acquired.child.reap_and_release()?.success());
    Ok(())
}
