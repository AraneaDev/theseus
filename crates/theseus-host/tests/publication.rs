use std::{
    ffi::OsStr,
    io::{self, Read, Write},
};
use theseus_host::fs::{CommitScope, HostDir, PublicationError, PublishStage, RelativePath};

fn bytes(root: &HostDir) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    root.open_file(&RelativePath::parse("report.json")?)?
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}
fn publish(root: &HostDir, data: &[u8]) -> io::Result<CommitScope> {
    root.publish(OsStr::new("report.json"), &mut |f| f.write_all(data))
}
fn detail(error: &io::Error) -> &PublicationError {
    error
        .get_ref()
        .and_then(|e| e.downcast_ref())
        .expect("typed publication failure")
}

#[test]
fn failed_writer_preserves_published_bytes() -> io::Result<()> {
    let parent = tempfile::tempdir()?;
    let root = HostDir::create_private(&parent.path().join("run"))?;
    publish(&root, b"old")?;
    let error = root
        .publish(OsStr::new("report.json"), &mut |f| {
            f.write_all(b"partial")?;
            Err(io::Error::other("injected writer failure"))
        })
        .unwrap_err();
    assert_eq!(detail(&error).stage(), PublishStage::Write);
    assert!(!detail(&error).replaced());
    assert_eq!(bytes(&root)?, b"old");
    assert_eq!(root.entries()?, vec!["report.json"]);
    Ok(())
}

#[test]
fn serializer_limit_preserves_previous_publication() -> io::Result<()> {
    let parent = tempfile::tempdir()?;
    let root = HostDir::create_private(&parent.path().join("run"))?;
    publish(&root, b"old")?;
    // The caller owns its schema/byte bound; publish must propagate its failure.
    let error = root
        .publish(OsStr::new("report.json"), &mut |f| {
            let limit = 4;
            let data = b"too large";
            f.write_all(&data[..limit])?;
            if data.len() > limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "serializer limit",
                ));
            }
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(bytes(&root)?, b"old");
    assert_eq!(root.entries()?, vec!["report.json"]);
    Ok(())
}

#[test]
fn replacement_preserves_retained_old_object_and_reports_platform_scope() -> io::Result<()> {
    let parent = tempfile::tempdir()?;
    let root = HostDir::create_private(&parent.path().join("run"))?;
    publish(&root, b"old")?;
    let mut old = root.open_file(&RelativePath::parse("report.json")?)?;
    let scope = publish(&root, b"new complete")?;
    #[cfg(unix)]
    assert_eq!(scope, CommitScope::FileAndDirectorySynced);
    #[cfg(windows)]
    assert_eq!(scope, CommitScope::FileFlushedAtomicReplace);
    let mut retained = Vec::new();
    old.read_to_end(&mut retained)?;
    assert_eq!(retained, b"old");
    assert_eq!(bytes(&root)?, b"new complete");
    assert_eq!(root.entries()?, vec!["report.json"]);
    Ok(())
}

#[test]
fn publication_uses_retained_directory_after_path_substitution() -> io::Result<()> {
    let parent = tempfile::tempdir()?;
    let path = parent.path().join("run");
    let root = HostDir::create_private(&path)?;
    publish(&root, b"old")?;
    std::fs::rename(&path, parent.path().join("moved"))?;
    let replacement = HostDir::create_private(&path)?;
    publish(&root, b"pinned")?;
    assert_eq!(bytes(&root)?, b"pinned");
    assert!(replacement.entries()?.is_empty());
    Ok(())
}

#[test]
fn invalid_basename_never_calls_writer_or_creates_temporary() -> io::Result<()> {
    let parent = tempfile::tempdir()?;
    let root = HostDir::create_private(&parent.path().join("run"))?;
    for name in ["", ".", "..", "a/b", "nul\0name"] {
        let mut called = false;
        let error = root
            .publish(OsStr::new(name), &mut |_| {
                called = true;
                Ok(())
            })
            .unwrap_err();
        assert!(!called);
        assert_eq!(detail(&error).stage(), PublishStage::Reserve);
        assert!(!detail(&error).replaced());
    }
    assert!(root.entries()?.is_empty());
    Ok(())
}

#[test]
fn failed_replace_keeps_destination_directory_and_unrelated_files() -> io::Result<()> {
    let parent = tempfile::tempdir()?;
    let root = HostDir::create_private(&parent.path().join("run"))?;
    root.create_child(OsStr::new("report.json"))?;
    root.create_file(&RelativePath::parse("unrelated")?)?
        .write_all(b"keep")?;
    let error = publish(&root, b"new").unwrap_err();
    assert_eq!(detail(&error).stage(), PublishStage::Replace);
    assert!(!detail(&error).replaced());
    root.open_child(&RelativePath::parse("report.json")?)?;
    assert_eq!(root.entries()?, vec!["report.json", "unrelated"]);
    assert_eq!(std::fs::read(parent.path().join("run/unrelated"))?, b"keep");
    Ok(())
}

#[test]
fn unwinding_writer_cleans_only_its_owned_temporary() -> io::Result<()> {
    let parent = tempfile::tempdir()?;
    let root = HostDir::create_private(&parent.path().join("run"))?;
    publish(&root, b"old")?;
    root.create_file(&RelativePath::parse("unrelated")?)?;
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = root.publish(OsStr::new("report.json"), &mut |_| panic!("writer panic"));
    }));
    assert!(panic.is_err());
    assert_eq!(bytes(&root)?, b"old");
    assert_eq!(root.entries()?, vec!["report.json", "unrelated"]);
    Ok(())
}

#[cfg(unix)]
#[test]
fn replacing_link_replaces_entry_without_touching_target() -> io::Result<()> {
    let parent = tempfile::tempdir()?;
    let root = HostDir::create_private(&parent.path().join("run"))?;
    let target = parent.path().join("outside");
    std::fs::write(&target, b"outside")?;
    std::os::unix::fs::symlink(&target, parent.path().join("run/report.json"))?;
    publish(&root, b"inside")?;
    assert_eq!(std::fs::read(target)?, b"outside");
    assert_eq!(bytes(&root)?, b"inside");
    Ok(())
}

#[cfg(windows)]
#[test]
fn sharing_violation_preserves_old_file_and_cleans_temporary() -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    let parent = tempfile::tempdir()?;
    let path = parent.path().join("run");
    let root = HostDir::create_private(&path)?;
    publish(&root, b"old")?;
    let retained = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path.join("report.json"))?;
    let error = publish(&root, b"new").unwrap_err();
    assert_eq!(detail(&error).stage(), PublishStage::Replace);
    assert!(!detail(&error).replaced());
    assert_eq!(bytes(&root)?, b"old");
    assert_eq!(root.entries()?, vec!["report.json"]);
    drop(retained);
    publish(&root, b"new")?;
    assert_eq!(bytes(&root)?, b"new");
    Ok(())
}

#[cfg(windows)]
#[test]
fn native_utf16_basename_is_preserved_and_aliases_rejected() -> io::Result<()> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    let parent = tempfile::tempdir()?;
    let root = HostDir::create_private(&parent.path().join("run"))?;
    let name = OsString::from_wide(&[0x72, 0xd800, 0x78]);
    root.publish(&name, &mut |f| f.write_all(b"native"))?;
    root.publish(&name, &mut |f| f.write_all(b"replacement"))?;
    assert_eq!(root.entries()?, vec![name.clone()]);
    assert_eq!(
        std::fs::read(parent.path().join("run").join(name))?,
        b"replacement"
    );
    for name in [
        "stream:ads",
        "CON",
        "NUL.txt",
        "COM1",
        "trailing.",
        "trailing ",
        "a\\b",
    ] {
        assert!(
            root.publish(OsStr::new(name), &mut |_| panic!("invalid writer"))
                .is_err()
        );
    }
    Ok(())
}

struct InterruptedFixture {
    child: std::process::Child,
    reader: Option<std::thread::JoinHandle<()>>,
}
impl Drop for InterruptedFixture {
    fn drop(&mut self) {
        // Termination closes the pipe even on timeout/assertion/unwind. Always reap
        // before joining the bounded line reader so no blocked thread is detached.
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
fn interrupt(root_path: &std::path::Path, gate: &str, expected: &str) -> io::Result<()> {
    use std::{
        io::BufRead,
        process::{Command, Stdio},
        sync::mpsc,
        time::Duration,
    };
    let mut child = Command::new(env!("CARGO_BIN_EXE_theseus-host-fixture"))
        .arg("publication-interruption")
        .arg(root_path)
        .arg(gate)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let output = child.stdout.take().expect("owned stdout");
    let mut fixture = InterruptedFixture {
        child,
        reader: None,
    };
    let (sender, receiver) = mpsc::channel();
    fixture.reader = Some(std::thread::spawn(move || {
        let mut line = String::new();
        let result = std::io::BufReader::new(output)
            .take(256)
            .read_line(&mut line)
            .map(|_| line);
        let _ = sender.send(result);
    }));
    let line = receiver
        .recv_timeout(Duration::from_secs(10))
        .map_err(|e| io::Error::new(io::ErrorKind::TimedOut, e))??;
    assert_eq!(line, expected);
    fixture.child.kill()?;
    let status = fixture.child.wait()?;
    assert!(!status.success(), "fixture must be terminated while gated");
    Ok(())
}

#[test]
fn process_interruption_before_replace_preserves_old_and_after_return_preserves_new()
-> io::Result<()> {
    let parent = tempfile::tempdir()?;
    let before_path = parent.path().join("before");
    let before = HostDir::create_private(&before_path)?;
    publish(&before, b"old")?;
    interrupt(&before_path, "before", "before:writer-callback\n")?;
    assert_eq!(bytes(&before)?, b"old");
    // Forced termination bypasses Rust cleanup. An uncommitted private sibling
    // can remain; this is a process-interruption test, never a power-loss test.
    let names = before.entries()?;
    assert_eq!(names.len(), 2);
    for name in names {
        if name != "report.json" {
            assert_eq!(
                std::fs::read(before_path.join(name))?,
                b"complete new bytes"
            );
        }
    }
    let after_path = parent.path().join("after");
    let after = HostDir::create_private(&after_path)?;
    publish(&after, b"old")?;
    #[cfg(unix)]
    let expected = "after:FileAndDirectorySynced\n";
    #[cfg(windows)]
    let expected = "after:FileFlushedAtomicReplace\n";
    interrupt(&after_path, "after", expected)?;
    assert_eq!(bytes(&after)?, b"complete new bytes");
    assert_eq!(after.entries()?, vec!["report.json"]);
    Ok(())
}

#[test]
fn interruption_fixture_eof_does_not_authorize_publication() -> io::Result<()> {
    use std::process::{Command, Stdio};
    let parent = tempfile::tempdir()?;
    let path = parent.path().join("run");
    let root = HostDir::create_private(&path)?;
    publish(&root, b"old")?;
    let child = Command::new(env!("CARGO_BIN_EXE_theseus-host-fixture"))
        .arg("publication-interruption")
        .arg(&path)
        .arg("before")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut fixture = InterruptedFixture {
        child,
        reader: None,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(status) = fixture.child.try_wait()? {
            assert!(!status.success());
            break;
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "fixture EOF did not exit",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(bytes(&root)?, b"old");
    assert_eq!(root.entries()?, vec!["report.json"]);
    Ok(())
}

#[test]
fn replacing_writer_file_cannot_redirect_temporary_cleanup() -> io::Result<()> {
    let parent = tempfile::tempdir()?;
    let root = HostDir::create_private(&parent.path().join("run"))?;
    publish(&root, b"old")?;
    let outside = parent.path().join("outside");
    std::fs::write(&outside, b"outside")?;
    let error = root
        .publish(OsStr::new("report.json"), &mut |file| {
            file.write_all(b"partial")?;
            *file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&outside)?;
            Err(io::Error::other("writer replaced its File"))
        })
        .unwrap_err();
    assert_eq!(detail(&error).stage(), PublishStage::Write);
    assert!(detail(&error).cleanup_error().is_none());
    assert_eq!(bytes(&root)?, b"old");
    assert_eq!(root.entries()?, vec!["report.json"]);
    assert_eq!(std::fs::read(outside)?, b"outside");
    Ok(())
}

#[cfg(windows)]
#[test]
fn readonly_destination_is_preserved_without_delete_fallback() -> io::Result<()> {
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Ok(mut permissions) = std::fs::metadata(&self.0).map(|m| m.permissions()) {
                permissions.set_readonly(false);
                let _ = std::fs::set_permissions(&self.0, permissions);
            }
        }
    }
    let parent = tempfile::tempdir()?;
    let path = parent.path().join("run");
    let root = HostDir::create_private(&path)?;
    publish(&root, b"old")?;
    let destination = path.join("report.json");
    let _restore = Restore(destination.clone());
    let mut permissions = std::fs::metadata(&destination)?.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&destination, permissions)?;
    let error = publish(&root, b"new").unwrap_err();
    assert_eq!(detail(&error).stage(), PublishStage::Replace);
    assert!(!detail(&error).replaced());
    assert!(detail(&error).cleanup_error().is_none());
    assert_eq!(bytes(&root)?, b"old");
    assert_eq!(root.entries()?, vec!["report.json"]);
    assert!(std::fs::metadata(destination)?.permissions().readonly());
    Ok(())
}
