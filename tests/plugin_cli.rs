//! Actual supplied native client, public CLI, retained reports. Never a replay fallback.
#[test]
#[ignore = "requires Docker and explicit THESEUS_PLUGIN_CLIENT + THESEUS_PLUGIN_CLIENT_VERSION; native file is executed only in the guest"]
fn native_public_cli_all_scopes_faults_timeout_and_cancel() {
    let output = std::process::Command::new("python3")
        .args([
            "tests/support/plugin_native.py",
            env!("CARGO_BIN_EXE_theseus"),
            "cli",
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
