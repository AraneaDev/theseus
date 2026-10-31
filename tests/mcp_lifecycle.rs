//! Requires explicit local vendor inputs. Ignored means UNRUN, never native PASS.
#[test]
#[ignore = "UNRUN without explicit THESEUS_MCP_CLAUDE_CLIENT, THESEUS_MCP_CODEX_CLIENT, THESEUS_MCP_TEST_IMAGE and reachable Docker"]
fn supplied_native_matrix_faults_and_strict_owned_cleanup() {
    let output = std::process::Command::new(if cfg!(windows) { "python" } else { "python3" })
        .args(["tests/support/mcp_native.py", env!("CARGO_BIN_EXE_theseus")])
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
