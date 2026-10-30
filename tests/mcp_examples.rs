//! Host-only harness oracles and generated fixtures, never native acceptance.
#[test]
fn independent_wire_and_cancel_oracles_reject_false_native_proof() {
    let output = std::process::Command::new("python3")
        .arg("tests/support/mcp_native_test.py")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
