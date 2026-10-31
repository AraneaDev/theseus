//! Trusted native peers shared by mixed integration suites.
#![allow(dead_code)]
use std::path::{Path, PathBuf};
pub fn docker(directory: &Path, mode: &str) -> PathBuf {
    let executable = directory.join(format!(
        "theseus-docker-{mode}{}",
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::copy(
        std::env::var_os("THESEUS_HOST_TEST_FIXTURE").expect("build required native fixture"),
        &executable,
    )
    .unwrap();
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
