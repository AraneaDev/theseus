//! Actual narrow public PTYs over the explicitly supplied native artifact.
#[test]
#[ignore = "requires Unix PTY, Docker and explicit THESEUS_PLUGIN_CLIENT + THESEUS_PLUGIN_CLIENT_VERSION"]
fn native_live_offline_proof_browsing_cancel_and_terminal_restoration() {
    let output = std::process::Command::new("python3")
        .args([
            "tests/support/plugin_native_pty.py",
            env!("CARGO_BIN_EXE_theseus"),
        ])
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
