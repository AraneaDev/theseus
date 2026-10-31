//! Required actual-binary native terminal acceptance (Unix PTY / Windows ConPTY).
#[test]
fn native_terminal_preview_restores_modes_and_drains_owned_session() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let output = std::process::Command::new(python)
        .arg("tests/support/native_terminal_test.py")
        .arg(env!("CARGO_BIN_EXE_theseus"))
        .output()
        .expect("required native Python");
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
    let output = std::process::Command::new(if cfg!(windows) { "python" } else { "python3" })
        .arg("tests/support/terminal_protocol_test.py")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
