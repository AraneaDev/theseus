use std::process::Command;

#[test]
fn public_preview_editing_pty() {
    let output = Command::new("python3")
        .arg("tests/support/preview_editing_pty.py")
        .arg(env!("CARGO_BIN_EXE_theseus"))
        .arg("preview")
        .output()
        .expect("run PTY acceptance");
    assert!(
        output.status.success(),
        "PTY acceptance failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires Docker and cached ubuntu:24.04 image"]
fn saved_source_executes_with_docker() {
    let output = Command::new("python3")
        .arg("tests/support/preview_editing_pty.py")
        .arg(env!("CARGO_BIN_EXE_theseus"))
        .arg("docker")
        .output()
        .expect("run saved source Docker acceptance");
    assert!(
        output.status.success(),
        "Docker acceptance failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
