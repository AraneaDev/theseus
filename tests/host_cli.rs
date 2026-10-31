//! Required native application CLI boundary, with a real no-execution peer.
use std::{path::Path, process::Command};
fn cli(root: &Path, bin: &Path, sentinel: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_theseus"));
    command
        .current_dir(root)
        .env("PATH", std::env::join_paths([bin]).unwrap())
        .env("THESEUS_DOCKER_SENTINEL", sentinel);
    command
}
#[test]
fn actual_native_cli_init_validate_and_offline_report_never_invoke_docker() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::copy(
        std::env::var_os("THESEUS_HOST_TEST_FIXTURE").expect("required native fixture"),
        bin.join(format!("docker{}", std::env::consts::EXE_SUFFIX)),
    )
    .unwrap();
    let sentinel = temp.path().join("docker-called");
    for argument in ["--help", "--version", "init", "validate"] {
        let result = cli(temp.path(), &bin, &sentinel)
            .arg(argument)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{argument}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let store = theseus::report::create(&temp.path().join("offline")).unwrap();
    store
        .save(&theseus::model::RunReport::new("native-offline-界"))
        .unwrap();
    let result = cli(temp.path(), &bin, &sentinel)
        .arg("report")
        .arg(store.root())
        .arg("--json")
        .output()
        .unwrap();
    assert!(result.status.success());
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["run_id"], "native-offline-界");
    let source = std::fs::read(temp.path().join("theseus.toml")).unwrap();
    let collision = cli(temp.path(), &bin, &sentinel)
        .arg("init")
        .output()
        .unwrap();
    assert_eq!(collision.status.code(), Some(2));
    assert_eq!(
        std::fs::read(temp.path().join("theseus.toml")).unwrap(),
        source
    );
    assert!(!sentinel.exists());
}
