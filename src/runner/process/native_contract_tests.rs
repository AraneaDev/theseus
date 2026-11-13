use super::*;

#[test]
fn native_launch_refuses_batch_suffix_before_fixture_execution() {
    let fixture = std::env::var_os("THESEUS_HOST_TEST_FIXTURE")
        .expect("build tests/support/build_host_fixture.py and set THESEUS_HOST_TEST_FIXTURE");
    let temp = tempfile::tempdir().unwrap();
    let executable = temp.path().join("owned-fixture.cmd");
    let marker = temp.path().join("must-not-execute");
    std::fs::copy(fixture, &executable).unwrap();
    let result = run(
        Command::new(executable).arg("launch-marker").arg(&marker),
        Duration::from_secs(2),
        &CancellationToken::default(),
        None,
        32,
        || Ok(()),
    );
    assert!(
        result.is_err(),
        "batch suffix must be rejected at the native command boundary"
    );
    assert!(
        !marker.exists(),
        "rejected command must never execute fixture code"
    );
}
