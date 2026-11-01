//! Trusted native peers shared by mixed integration suites.
#![allow(dead_code)]
use std::path::{Path, PathBuf};
pub fn docker(directory: &Path, mode: &str) -> PathBuf {
    let executable = directory.join(format!(
        "theseus-docker-{mode}{}",
        std::env::consts::EXE_SUFFIX
    ));
    let source = PathBuf::from(
        std::env::var_os("THESEUS_HOST_TEST_FIXTURE").expect("build required native fixture"),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // These aliases only execute or rename/unlink their own directory entry.
        // Never write/chmod the shared pinned inode. Unlike copying, publication
        // opens no executable writer for a concurrent Unix fork to inherit.
        let pinned = std::fs::symlink_metadata(&source).unwrap();
        assert!(
            pinned.is_file(),
            "pinned native fixture must be a regular file"
        );
        match std::fs::hard_link(&source, &executable) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => panic!(
                "immutable native aliases require the pinned fixture and test TempDir on the same filesystem (use the same TMPDIR): {error}"
            ),
        }
        let alias = std::fs::symlink_metadata(&executable).unwrap();
        assert!(
            alias.is_file() && (alias.dev(), alias.ino()) == (pinned.dev(), pinned.ino()),
            "native alias must be a regular non-symlink to the pinned fixture inode"
        );
    }
    // Windows has explicit launch handle lists and different loaded-image rename
    // semantics. Preserve its independent copied executable behavior.
    #[cfg(windows)]
    std::fs::copy(&source, &executable).unwrap();
    executable
}
#[cfg(unix)]
pub fn symlink(source: impl AsRef<Path>, target: impl AsRef<Path>) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, target)
}
#[cfg(windows)]
pub fn symlink(source: impl AsRef<Path>, target: impl AsRef<Path>) -> std::io::Result<()> {
    if source.as_ref().is_dir() {
        std::os::windows::fs::symlink_dir(source, target)
    } else {
        std::os::windows::fs::symlink_file(source, target)
    }
}
pub fn status(code: i32) -> std::process::ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code << 8)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code as u32)
    }
}
pub fn shell(command: &str) -> std::process::Command {
    #[cfg(unix)]
    {
        let mut c = std::process::Command::new("/bin/sh");
        c.args(["-c", command]);
        c
    }
    #[cfg(windows)]
    {
        let executable =
            PathBuf::from(std::env::var_os("SystemRoot").expect("native Windows system root"))
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut c = std::process::Command::new(executable);
        c.args(["-NoProfile", "-NonInteractive", "-Command", command]);
        c
    }
}
