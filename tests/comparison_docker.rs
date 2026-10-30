//! Public CLI acceptance: every external resource belongs to the Python fixture.
fn acceptance(case: &str) {
    let output = std::process::Command::new("python3")
        .args([
            "tests/support/comparison_pty.py",
            env!("CARGO_BIN_EXE_theseus"),
            case,
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
#[test]
#[ignore = "requires Docker with local ubuntu:24.04"]
fn copy_archive_same_artifact_both_profiles() {
    acceptance("methods");
}
#[test]
#[ignore = "requires Docker with local ubuntu:24.04"]
fn two_distinct_actual_image_ids_both_profiles() {
    acceptance("images");
}
#[test]
#[ignore = "requires Docker with local ubuntu:24.04"]
fn deliberate_scoped_difference_both_profiles() {
    acceptance("difference");
}
#[test]
#[ignore = "requires Docker with local ubuntu:24.04"]
fn failed_source_prevents_suite_success_both_profiles() {
    acceptance("failed");
}
#[test]
#[ignore = "requires Docker with local ubuntu:24.04"]
fn headless_cancellation_each_profile_prevents_next_variant_and_cleans_up() {
    acceptance("cancel");
}
#[test]
#[ignore = "requires Docker and Unix PTY"]
fn real_pty_comparison_preview_live_offline_cancel_and_restore() {
    acceptance("pty");
}

#[test]
fn fallback_cleanup_retains_diagnostics_and_original_assertion() {
    let output = std::process::Command::new("python3")
        .arg("tests/support/test_comparison_cleanup.py")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
