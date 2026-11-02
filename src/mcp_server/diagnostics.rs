//! Serve deliberately omits optional, unqualified stderr diagnostics.
use std::panic::{AssertUnwindSafe, catch_unwind};
/// Install before native acquisition or worker creation at the binary boundary.
/// There is deliberately no stderr/stdout handle, lock, formatting or callback.
pub(crate) fn install_quiet_panic_hook() {
    std::panic::set_hook(Box::new(|_| {}));
}
/// Keep original owners outside this unwind boundary. The caller retains the
/// returned typed panic and defers cleanup inspection to its finalizer worker.
pub(crate) fn guard<T>(encode: impl FnOnce() -> T) -> Result<T, anyhow::Error> {
    match catch_unwind(AssertUnwindSafe(encode)) {
        Ok(value) => Ok(value),
        Err(payload) => Err(crate::runner::process::worker_panic(payload)),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Witness(Arc<AtomicUsize>);
    impl Drop for Witness {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[test]
    fn serializer_panic_retains_opaque_payload_until_error_owner_drops() {
        let drops = Arc::new(AtomicUsize::new(0));
        let result: Result<(), _> = guard(|| std::panic::panic_any(Witness(drops.clone())));
        assert!(result.is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(result);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}
#[cfg(test)]
mod quiet_hook_tests {
    use super::*;
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    fn child() -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "mcp_server::diagnostics::quiet_hook_tests::quiet_hook_omits_aliased_and_blocked_stderr", "--nocapture"])
            .env("THESEUS_MCP_QUIET_PANIC_TEST", "child").stdin(Stdio::null());
        command
    }
    fn supervise(mut command: Command) {
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("quiet panic hook blocked supervised child");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn quiet_hook_omits_aliased_and_blocked_stderr() {
        if std::env::var_os("THESEUS_MCP_QUIET_PANIC_TEST").is_some() {
            install_quiet_panic_hook();
            assert!(guard(|| panic!("forbidden panic diagnostic")).is_err());
            return;
        }
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut command = child();
        command
            .stdout(Stdio::from(file.as_file().try_clone().unwrap()))
            .stderr(Stdio::from(file.as_file().try_clone().unwrap()));
        supervise(command);
        assert!(
            !String::from_utf8_lossy(&std::fs::read(file.path()).unwrap())
                .contains("forbidden panic diagnostic")
        );
        #[cfg(unix)]
        {
            use std::{
                io::Write,
                os::{fd::OwnedFd, unix::net::UnixStream},
            };
            let (_unread, mut output) = UnixStream::pair().unwrap();
            output.set_nonblocking(true).unwrap();
            loop {
                match output.write(&[0; 4096]) {
                    Ok(n) => assert!(n > 0),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => panic!("fill stderr socket: {e}"),
                }
            }
            output.set_nonblocking(false).unwrap();
            let mut command = child();
            command
                .stdout(Stdio::null())
                .stderr(Stdio::from(OwnedFd::from(output)));
            supervise(command);
        }
    }
}
