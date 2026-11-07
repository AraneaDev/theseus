mod common;
use common::*;
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus},
    time::{Duration, Instant},
};
use theseus_native_helper_store::*;

static PARENT_PROBES: std::sync::Mutex<()> = std::sync::Mutex::new(());
// Child creation is serialized within this binary so fork cannot temporarily
// retain a different parent test's ledger lock before exec.
// Same retained-child/monotonic-deadline pattern as host native_helper_ledger.
struct Joined {
    process: Child,
    status: Option<ExitStatus>,
    receipt: PathBuf,
    expected: String,
    logs: [PathBuf; 2],
}
impl Joined {
    fn try_status(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_none() {
            self.status = self.process.try_wait()?;
        }
        Ok(self.status)
    }
    fn completed(&self) -> bool {
        if self.status.is_none() {
            return false;
        }
        let mut bytes = Vec::new();
        File::open(&self.receipt)
            .and_then(|f| f.take(257).read_to_end(&mut bytes))
            .is_ok()
            && bytes == self.expected.as_bytes()
    }
    fn diagnostics(&self) -> String {
        let mut message = String::new();
        for path in &self.logs {
            let mut bytes = Vec::new();
            if let Ok(file) = File::open(path) {
                let _ = file.take(4096).read_to_end(&mut bytes);
            }
            message.push_str(&format!(
                "child diagnostic {:?}\n",
                String::from_utf8_lossy(&bytes)
            ));
        }
        message
    }
    fn wait_verified(&mut self, timeout: Duration) -> Result<(), &'static str> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.try_status() {
                Ok(Some(status)) => {
                    if !status.success() {
                        return Err("child failed");
                    }
                    if !self.completed() {
                        return Err("child completion receipt missing");
                    }
                    return Ok(());
                }
                Err(_) => {
                    self.cleanup_with(Child::kill);
                    return Err("child status failed");
                }
                Ok(None) => (),
            }
            if Instant::now() >= deadline {
                self.cleanup_with(Child::kill);
                return Err("child timeout");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn cleanup_with(&mut self, kill: impl FnOnce(&mut Child) -> io::Result<()>) {
        if matches!(self.try_status(), Ok(Some(_))) {
            return;
        }
        let kill_error = kill(&mut self.process).err();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if matches!(self.try_status(), Ok(Some(_))) {
                return;
            }
            if Instant::now() >= deadline {
                eprintln!(
                    "unresolved test child owner retained; kill error {kill_error:?}; {}",
                    self.diagnostics()
                );
                // Retain the actual Child owner until forced nonzero process exit.
                // Never unwind, drop the unresolved owner or claim a successful join.
                std::process::exit(98);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Joined {
    fn drop(&mut self) {
        self.cleanup_with(Child::kill);
    }
}
fn child(f: &Fixture, mode: &str, entry: &str) -> Joined {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let receipt = f.tmp.path().join(format!("receipt-{nonce}"));
    let logs = [
        f.tmp.path().join(format!("stdout-{nonce}")),
        f.tmp.path().join(format!("stderr-{nonce}")),
    ];
    let process = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", entry, "--ignored", "--nocapture"])
        .env("THESEUS_TEST_LEDGER", f.tmp.path().join("ledger"))
        .env("THESEUS_TEST_MODE", mode)
        .env("THESEUS_TEST_RECEIPT", &receipt)
        .env("THESEUS_TEST_NONCE", &nonce)
        .stdout(File::create_new(&logs[0]).unwrap())
        .stderr(File::create_new(&logs[1]).unwrap())
        .spawn()
        .unwrap();
    Joined {
        process,
        status: None,
        receipt,
        expected: format!("lock_probe:{nonce}"),
        logs,
    }
}
#[test]
fn native_process_lock_is_retained_until_store_drop() {
    let _serial = PARENT_PROBES.lock().unwrap();
    let f = Fixture::new();
    let b = f.bootstrap();
    let s = LedgerStore::open(&f.ledger, &f.workspace, f.binding(b.workspace_marker())).unwrap();
    let mut blocked = child(&f, "blocked", "lock_probe");
    let result = blocked.wait_verified(Duration::from_secs(10));
    assert_eq!(result, Ok(()), "{}", blocked.diagnostics());
    drop(s);
    let mut released = child(&f, "released", "lock_probe");
    let result = released.wait_verified(Duration::from_secs(10));
    assert_eq!(result, Ok(()), "{}", released.diagnostics());
}
#[test]
fn zero_selected_exit_zero_is_not_a_completion_receipt() {
    let _serial = PARENT_PROBES.lock().unwrap();
    let f = Fixture::new();
    let mut child = child(&f, "released", "nonexistent_probe");
    assert_eq!(
        child.wait_verified(Duration::from_secs(10)),
        Err("child completion receipt missing")
    );
    assert!(child.status.unwrap().success());
    assert!(!child.completed());
    for path in &child.logs {
        let mut bytes = vec![b'a'; 4096];
        bytes.extend_from_slice(b"OVERBOUND");
        std::fs::write(path, bytes).unwrap();
    }
    let diagnostics = child.diagnostics();
    assert!(!diagnostics.contains("OVERBOUND"));
    assert!(diagnostics.len() < 8300);
}
#[test]
fn timeout_kills_and_joins_before_returning_without_receipt() {
    let _serial = PARENT_PROBES.lock().unwrap();
    let f = Fixture::new();
    let mut child = child(&f, "idle", "lock_probe");
    let start = Instant::now();
    assert_eq!(
        child.wait_verified(Duration::from_millis(50)),
        Err("child timeout")
    );
    assert!(start.elapsed() < Duration::from_secs(4));
    assert!(child.status.is_some());
    assert!(!child.completed());
}
#[test]
fn kill_error_keeps_owner_and_observes_eventual_join() {
    let _serial = PARENT_PROBES.lock().unwrap();
    let f = Fixture::new();
    let mut child = child(&f, "delayed", "lock_probe");
    let ready = child.receipt.with_extension("ready");
    let release = child.receipt.with_extension("release");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "child readiness deadline");
        assert!(
            child.try_status().unwrap().is_none(),
            "child exited before readiness"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // Deliberately pause beyond the former 100ms race window: readiness proves
    // the child started, but only the kill-error closure may release it.
    std::thread::sleep(Duration::from_millis(250));
    assert!(
        child.try_status().unwrap().is_none(),
        "child exited before closure release"
    );
    assert!(!child.completed());
    assert!(!release.exists());
    let mut attempted = false;
    child.cleanup_with(|_| {
        attempted = true;
        std::fs::write(&release, b"release").unwrap();
        Err(io::Error::other("modeled kill failure"))
    });
    assert!(attempted);
    assert!(child.status.unwrap().success());
    assert!(child.completed());
}
#[test]
#[ignore = "fixed bounded subprocess entry point"]
fn lock_probe() {
    use theseus_host::fs::{HostDir, RootPolicy};
    let path = std::env::var_os("THESEUS_TEST_LEDGER").expect("test-owned root");
    match std::env::var("THESEUS_TEST_MODE").unwrap().as_str() {
        "idle" => {
            std::thread::sleep(Duration::from_secs(15));
            panic!("idle control must be killed");
        }
        "delayed" => {
            let receipt = PathBuf::from(std::env::var_os("THESEUS_TEST_RECEIPT").unwrap());
            std::fs::write(receipt.with_extension("ready"), b"ready").unwrap();
            let release = receipt.with_extension("release");
            let deadline = Instant::now() + Duration::from_secs(10);
            while !release.exists() {
                assert!(Instant::now() < deadline, "parent release deadline");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        mode => {
            let dir = HostDir::open(Path::new(&path), RootPolicy::LeafNoFollow).unwrap();
            let result = dir.open_locked_ledger(std::ffi::OsStr::new("ledger.bin"));
            match mode {
                "blocked" => assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock),
                "released" => assert!(result.is_ok()),
                _ => panic!("unknown probe mode"),
            }
        }
    }
    let nonce = std::env::var("THESEUS_TEST_NONCE").unwrap();
    std::fs::write(
        std::env::var_os("THESEUS_TEST_RECEIPT").unwrap(),
        format!("lock_probe:{nonce}"),
    )
    .unwrap();
}
