#![forbid(unsafe_code)]
//! Normal explicit serving entry point. No fixture modes or environment controls.
use std::{
    any::Any,
    ffi::OsString,
    io::Write,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    time::{Duration, Instant},
};
use theseus_host::stdio::{AcquireFailure, Stdio};
use theseus_native_helper::{Poll, Runtime, RuntimeError, StartFailure, load_config};
const CLEANUP_BUDGET: Duration = Duration::from_secs(5);
fn config_path(args: impl IntoIterator<Item = OsString>) -> Option<PathBuf> {
    let mut args = args.into_iter();
    args.next()?;
    if args.next()?.to_str()? != "serve" || args.next()?.to_str()? != "--config" {
        return None;
    }
    let path = args.next()?;
    let text = path.to_str()?;
    if args.next().is_some()
        || text.len() > 16384
        || text.contains('\0')
        || !std::path::Path::new(text).is_absolute()
    {
        return None;
    }
    Some(path.into())
}
#[derive(Clone, Copy)]
enum Diagnostic {
    Invalid,
    Startup,
    Runtime,
    Cleanup,
}
impl Diagnostic {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Invalid => b"helper: invalid invocation\n",
            Self::Startup => b"helper: startup failed\n",
            Self::Runtime => b"helper: runtime failed\n",
            Self::Cleanup => b"helper: cleanup incomplete\n",
        }
    }
}
fn fail(diagnostic: Diagnostic) -> ! {
    // Formatting is deliberately absent. A stderr failure is not a second diagnostic.
    let _ = std::io::stderr().lock().write_all(diagnostic.bytes());
    std::process::exit(2)
}
trait StopOwner {
    type Error;
    fn stop(&mut self);
    fn settle(&mut self) -> Result<bool, Self::Error>;
}
impl StopOwner for Runtime {
    type Error = RuntimeError;
    fn stop(&mut self) {
        self.request_stop()
    }
    fn settle(&mut self) -> Result<bool, Self::Error> {
        self.poll_settle()
    }
}
impl StopOwner for StartFailure {
    type Error = RuntimeError;
    fn stop(&mut self) {
        self.request_stop()
    }
    fn settle(&mut self) -> Result<bool, Self::Error> {
        self.poll_settle()
    }
}
impl StopOwner for AcquireFailure {
    type Error = std::io::Error;
    fn stop(&mut self) {
        self.request_stop()
    }
    fn settle(&mut self) -> Result<bool, Self::Error> {
        self.poll_stop_or_restore()
    }
}
// `owner` remains on its caller's stack until actual settlement or process exit.
// The policy is cooperative: any individual native/kernel call may block longer.
fn settle<T: StopOwner>(owner: &mut T) -> Option<T::Error> {
    owner.stop();
    let began = Instant::now();
    let mut first_error = None;
    loop {
        match catch_unwind(AssertUnwindSafe(|| owner.settle())) {
            Ok(Ok(true)) => return first_error,
            Ok(Ok(false)) => {}
            Ok(Err(error)) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
            Err(payload) => {
                let _retained_payload = payload;
                fail(Diagnostic::Cleanup)
            }
        }
        if began.elapsed() >= CLEANUP_BUDGET {
            fail(Diagnostic::Cleanup)
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn serve(mut runtime: Runtime) {
    let mut primary_error = None;
    let mut panic_payload: Option<Box<dyn Any + Send>> = None;
    loop {
        match catch_unwind(AssertUnwindSafe(|| runtime.poll())) {
            Ok(Ok(Poll::Settled)) => return,
            Ok(Ok(Poll::Progress | Poll::Pending)) => {}
            Ok(Err(error)) => {
                primary_error = Some(error);
                break;
            }
            Err(payload) => {
                panic_payload = Some(payload);
                break;
            }
        }
        if runtime.is_stopping() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let cleanup_error = settle(&mut runtime);
    if primary_error.is_some() || panic_payload.is_some() || cleanup_error.is_some() {
        fail(Diagnostic::Runtime)
    }
}
fn main() {
    // First operation, before args/config, native stdio or worker acquisition.
    std::panic::set_hook(Box::new(|_| {}));
    let Some(path) = config_path(std::env::args_os()) else {
        fail(Diagnostic::Invalid)
    };
    let invocation = match load_config(&path) {
        Ok(v) => v,
        Err(error) => {
            let _retained_error = error;
            fail(Diagnostic::Invalid)
        }
    };
    let stdio = match Stdio::acquire() {
        Ok(v) => v,
        Err(mut failure) => {
            let _cleanup_error = settle(&mut failure);
            fail(Diagnostic::Startup)
        }
    };
    match Runtime::start(invocation, stdio) {
        Ok(runtime) => serve(runtime),
        Err(mut failure) => {
            let _cleanup_error = settle(&mut failure);
            fail(Diagnostic::Startup)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }
    #[test]
    fn cli_accepts_only_exact_explicit_absolute_utf8_config() {
        let p = std::env::temp_dir().join("explicit-config.json");
        let argv = vec![
            OsString::from("helper"),
            OsString::from("serve"),
            OsString::from("--config"),
            p.clone().into_os_string(),
        ];
        assert_eq!(config_path(argv), Some(p));
        for v in [
            &["helper"][..],
            &["helper", "--help"],
            &["helper", "serve"],
            &["helper", "serve", "--config", "relative"],
            &["helper", "serve", "--config", "/a", "extra"],
            &["helper", "serve", "--config", "/a", "--config", "/b"],
            &["helper", "serve", "--config=/a"],
        ] {
            assert_eq!(config_path(args(v)), None);
        }
    }
    #[cfg(unix)]
    #[test]
    fn cli_rejects_non_utf8_without_lossy_conversion() {
        use std::os::unix::ffi::OsStringExt;
        assert_eq!(
            config_path(vec![
                "helper".into(),
                "serve".into(),
                "--config".into(),
                OsString::from_vec(vec![b'/', 255])
            ]),
            None
        );
    }
}
