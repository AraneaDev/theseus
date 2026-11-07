use std::{
    ffi::OsStr,
    io,
    path::Path,
    process::{Child, Command},
    time::{Duration, Instant},
};
use theseus_host::fs::{HostDir, RootPolicy};

struct Fixture {
    _temp: tempfile::TempDir,
    path: std::path::PathBuf,
}
impl Fixture {
    fn path(&self) -> &Path {
        &self.path
    }
}
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("private");
    HostDir::create_private(&path).unwrap();
    Fixture { _temp: temp, path }
}
fn root(path: &Path) -> HostDir {
    HostDir::open(path, RootPolicy::LeafNoFollow).unwrap()
}

#[test]
fn existing_only_regular_and_component_validation() {
    let t = fixture();
    let r = root(t.path());
    assert_eq!(
        r.open_locked_ledger(OsStr::new("missing"))
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert!(!t.path().join("missing").exists());
    for name in ["", ".", "..", "a/b", "/absolute"] {
        assert_eq!(
            r.open_locked_ledger(OsStr::new(name)).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    std::fs::create_dir(t.path().join("directory")).unwrap();
    assert!(r.open_locked_ledger(OsStr::new("directory")).is_err());
    std::fs::write(t.path().join("ledger"), b"original").unwrap();
    let mut f = r.open_locked_ledger(OsStr::new("ledger")).unwrap();
    assert_eq!(f.read_bounded(8).unwrap(), b"original");
    drop(f);
    assert_eq!(std::fs::read(t.path().join("ledger")).unwrap(), b"original");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("ledger", t.path().join("link")).unwrap();
        assert!(r.open_locked_ledger(OsStr::new("link")).is_err());
    }
    #[cfg(windows)]
    {
        // Native acceptance requires this test-owned symlink fixture capability;
        // lack of that capability fails visibly instead of silently skipping.
        std::os::windows::fs::symlink_file("ledger", t.path().join("link")).unwrap();
        assert!(r.open_locked_ledger(OsStr::new("link")).is_err());
    }
}

#[test]
fn bounds_refusals_preserve_bytes_and_do_not_poison() {
    let t = fixture();
    std::fs::write(t.path().join("ledger"), b"").unwrap();
    let mut f = root(t.path())
        .open_locked_ledger(OsStr::new("ledger"))
        .unwrap();
    assert!(f.read_bounded(0).unwrap().is_empty());
    f.append_and_sync(b"abc", 3).unwrap();
    assert_eq!(f.read_bounded(3).unwrap(), b"abc");
    assert!(f.read_bounded(2).is_err());
    assert!(f.read_bounded(0).is_err());
    assert_eq!(
        f.read_bounded(usize::MAX).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(f.append_and_sync(b"d", 3).is_err());
    assert_eq!(f.read_bounded(3).unwrap(), b"abc");
    f.append_and_sync(b"d", 4).unwrap();
    f.append_and_sync(b"", 4).unwrap();
    assert_eq!(f.read_bounded(4).unwrap(), b"abcd");
    drop(f);
    assert_eq!(std::fs::read(t.path().join("ledger")).unwrap(), b"abcd");
}

struct Joined {
    process: Child,
    status: Option<std::process::ExitStatus>,
    receipt: std::path::PathBuf,
    nonce: String,
    logs: [std::path::PathBuf; 2],
}
impl Joined {
    fn try_status(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        if self.status.is_none() {
            self.status = self.process.try_wait()?;
        }
        Ok(self.status)
    }
    fn completed(&self) -> bool {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(&self.receipt)
            .and_then(|file| file.take(257).read_to_end(&mut bytes))
            .is_ok()
            && bytes == self.nonce.as_bytes()
    }
    fn diagnostics(&self) {
        use std::io::Read;
        for path in &self.logs {
            let mut bytes = Vec::new();
            if let Ok(file) = std::fs::File::open(path) {
                let _ = file.take(4096).read_to_end(&mut bytes);
                for line in String::from_utf8_lossy(&bytes).lines() {
                    eprintln!("ledger child diagnostic: {line}");
                }
            }
        }
    }
    fn wait(mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.try_status().unwrap() {
                if !status.success() || !self.completed() {
                    self.diagnostics();
                }
                assert!(status.success(), "ledger child failed: {status}");
                assert!(
                    self.completed(),
                    "fixed child entry completion receipt missing"
                );
                return;
            }
            assert!(Instant::now() < deadline, "child timeout");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn cleanup_with(&mut self, kill: impl FnOnce(&mut Child) -> io::Result<()>) {
        if matches!(self.try_status(), Ok(Some(_))) {
            return;
        }
        // A failed kill does not relinquish the original Child or skip joining.
        let kill_error = kill(&mut self.process).err();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if matches!(self.try_status(), Ok(Some(_))) {
                return;
            }
            if Instant::now() >= deadline {
                self.diagnostics();
                eprintln!(
                    "ledger child cleanup deadline expired; owner still live; kill error: {kill_error:?}"
                );
                // Never unwind into a second wait, or claim the live child joined.
                // Exit is nonzero while this original Child owner is still retained.
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
fn child_entry(path: &Path, name: &str, mode: &str, entry: &str) -> Joined {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let receipt = path.join(format!("receipt-{nonce}"));
    let logs = [
        path.join(format!("stdout-{nonce}")),
        path.join(format!("stderr-{nonce}")),
    ];
    let stdout = std::fs::File::create_new(&logs[0]).unwrap();
    let stderr = std::fs::File::create_new(&logs[1]).unwrap();
    let process = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", entry, "--nocapture"])
        .env("THESEUS_LEDGER_TEST_ROOT", path)
        .env("THESEUS_LEDGER_TEST_NAME", name)
        .env("THESEUS_LEDGER_TEST_MODE", mode)
        .env("THESEUS_LEDGER_TEST_RECEIPT", &receipt)
        .env("THESEUS_LEDGER_TEST_NONCE", &nonce)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .unwrap();
    Joined {
        process,
        status: None,
        receipt,
        nonce,
        logs,
    }
}
fn child(path: &Path, name: &str, mode: &str) -> Joined {
    child_entry(path, name, mode, "ledger_child")
}
#[test]
#[ignore = "fixed bounded subprocess entry point"]
fn ledger_child() {
    let path = std::env::var_os("THESEUS_LEDGER_TEST_ROOT").expect("test-owned root");
    let name = std::env::var_os("THESEUS_LEDGER_TEST_NAME").unwrap();
    let mode = std::env::var("THESEUS_LEDGER_TEST_MODE").unwrap();
    if mode == "idle" {
        std::fs::write(Path::new(&path).join("ready"), b"ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !Path::new(&path).join("release").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    } else {
        let result = root(Path::new(&path)).open_locked_ledger(&name);
        if mode == "busy" {
            assert_eq!(result.err().unwrap().kind(), io::ErrorKind::WouldBlock);
        } else {
            assert!(result.is_ok(), "{result:?}");
        }
    }
    let receipt = std::env::var_os("THESEUS_LEDGER_TEST_RECEIPT").unwrap();
    let nonce = std::env::var("THESEUS_LEDGER_TEST_NONCE").unwrap();
    std::fs::write(receipt, nonce.as_bytes()).unwrap();
}
#[test]
fn cross_process_contention_alias_release_and_noninheritance() {
    let t = fixture();
    std::fs::write(t.path().join("ledger"), b"existing").unwrap();
    std::fs::hard_link(t.path().join("ledger"), t.path().join("alias")).unwrap();
    let f = root(t.path())
        .open_locked_ledger(OsStr::new("ledger"))
        .unwrap();
    child(t.path(), "ledger", "busy").wait();
    child(t.path(), "alias", "busy").wait();
    let mut idle = child(t.path(), "ledger", "idle");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !t.path().join("ready").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(idle.try_status().unwrap().is_none());
    drop(f);
    child(t.path(), "ledger", "available").wait();
    assert!(idle.try_status().unwrap().is_none());
    assert!(t.path().join("ready").exists());
    std::fs::write(t.path().join("release"), b"release").unwrap();
    idle.wait();
}
#[test]
fn retained_root_and_lock_follow_objects_not_replaced_names() {
    let t = fixture();
    let p = t.path().join("root");
    std::fs::create_dir(&p).unwrap();
    std::fs::write(p.join("ledger"), b"original").unwrap();
    let r = root(&p);
    std::fs::rename(&p, t.path().join("moved")).unwrap();
    std::fs::create_dir(&p).unwrap();
    std::fs::write(p.join("ledger"), b"replacement root").unwrap();
    let mut old = r.open_locked_ledger(OsStr::new("ledger")).unwrap();
    assert_eq!(old.read_bounded(8).unwrap(), b"original");
    child(&p, "ledger", "available").wait();
    let moved = t.path().join("moved");
    std::fs::rename(moved.join("ledger"), moved.join("old")).unwrap();
    std::fs::write(moved.join("ledger"), b"replacement file").unwrap();
    child(&moved, "old", "busy").wait();
    child(&moved, "ledger", "available").wait();
    assert_eq!(old.read_bounded(8).unwrap(), b"original");
}

#[test]
fn cleanup_kill_error_still_observes_actual_join() {
    let t = fixture();
    let mut idle = child(t.path(), "ledger", "idle");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !t.path().join("ready").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let release = t.path().join("release");
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(release, b"release").unwrap();
    });
    idle.cleanup_with(|_| Err(io::Error::other("test-only kill failure")));
    releaser.join().unwrap();
    assert!(idle.status.unwrap().success());
    assert!(idle.completed());
}
#[test]
fn zero_selected_child_tests_do_not_forge_completion() {
    let t = fixture();
    let mut zero = child_entry(t.path(), "ledger", "available", "nonexistent_fixed_entry");
    let deadline = Instant::now() + Duration::from_secs(5);
    while zero.try_status().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(zero.status.unwrap().success());
    assert!(
        !zero.completed(),
        "zero tests cannot mint the fixed-entry receipt"
    );
}
