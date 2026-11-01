//! Required actual-binary native terminal acceptance (Unix PTY / Windows ConPTY).
fn diagnostic_output(command: &mut std::process::Command) -> std::process::Output {
    if std::env::var_os("THESEUS_TERMINAL_TRACE").as_deref() == Some(std::ffi::OsStr::new("1")) {
        // Stream only the outer Python process. Helper JSON remains owned/piped
        // inside Python, so diagnostics cannot corrupt the paired protocol.
        command
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());
    }
    command.stdin(std::process::Stdio::null());
    command.output().expect("required native Python")
}
#[test]
fn native_terminal_preview_restores_modes_and_drains_owned_session() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let output = diagnostic_output(
        std::process::Command::new(python)
            .arg("tests/support/native_terminal_test.py")
            .arg(env!("CARGO_BIN_EXE_theseus")),
    );
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn terminal_protocol_rejects_malformed_bounds_and_joins_on_eof() {
    let output = diagnostic_output(
        std::process::Command::new(if cfg!(windows) { "python" } else { "python3" })
            .arg("tests/support/terminal_protocol_test.py")
            .env("PYTHONDONTWRITEBYTECODE", "1"),
    );
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
