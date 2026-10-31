#[path = "support/native.rs"]
mod native;
#[allow(dead_code)]
#[path = "support/comparison.rs"]
mod support;
use std::{fs, process::Command};
fn cli() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_theseus"));
    c.env("PATH", "/no-docker");
    c
}
#[test]
fn valid_failed_suite_json_is_recomputed_offline_and_exits_zero() {
    let t = tempfile::tempdir().unwrap();
    let root = support::saved(t.path());
    let out = cli()
        .args(["comparison", root.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r["outcome"], "failed");
    assert_eq!(r["cells"][0]["comparison"]["status"], "equivalent");
    assert_eq!(r["sources"][1]["image_id"], "sha256:archive");
}
#[test]
fn replaced_source_is_rejected_instead_of_using_cached_success() {
    let t = tempfile::tempdir().unwrap();
    let root = support::saved(t.path());
    fs::write(root.join("sources/archive/run-archive/report.json"), "{}").unwrap();
    let out = cli()
        .args(["comparison", root.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("digest mismatch"));
    assert!(out.stdout.is_empty());
}
#[test]
fn interactive_compare_requires_tty_before_creating_evidence() {
    let t = tempfile::tempdir().unwrap();
    let path = support::recipe_suite(t.path());
    let out = cli()
        .current_dir(t.path())
        .args(["compare", "--suite", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("requires a terminal"));
    assert!(!t.path().join(".theseus-comparisons").exists());
    assert!(!t.path().join(".theseus-runs").exists());
    assert!(!out.stderr.contains(&0x1b));
}
#[test]
fn cleanup_routes_suite_directory_and_fixed_file_through_validated_store() {
    let t = tempfile::tempdir().unwrap();
    let root = support::saved(t.path());
    for p in [root.clone(), root.join("comparison.json")] {
        let out = cli()
            .args(["cleanup", p.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut a: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("comparison.json")).unwrap()).unwrap();
    a["sources"][0]["path"] = "../escape".into();
    fs::write(
        root.join("comparison.json"),
        serde_json::to_vec(&a).unwrap(),
    )
    .unwrap();
    let out = cli()
        .args(["cleanup", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unsafe source reference"));
}
#[test]
fn headless_compare_records_single_source_failure_and_suite_recovery_without_docker() {
    let t = tempfile::tempdir().unwrap();
    let path = support::recipe_suite(t.path());
    let out = cli()
        .current_dir(t.path())
        .args([
            "compare",
            "--suite",
            path.to_str().unwrap(),
            "--headless",
            "--pull",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let roots = fs::read_dir(t.path().join(".theseus-comparisons"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(roots.len(), 1);
    let root = roots[0].path();
    let r = theseus::comparison::load_report(&root).unwrap();
    assert_eq!(r.outcome, theseus::model::RunOutcome::Incomplete);
    assert!(r.sources[0].report_path.is_some());
    assert!(r.sources[0].error.is_some());
    assert!(r.sources[1].report_path.is_none());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(error.contains(&root.display().to_string()));
    assert!(error.contains(&format!(
        "sources{}copy{}",
        std::path::MAIN_SEPARATOR,
        std::path::MAIN_SEPARATOR
    )));
    assert!(!out.stderr.contains(&0x1b));
    assert!(!t.path().join(".theseus-runs").exists());
}

#[test]
fn headless_recovery_command_preserves_quoted_root() {
    let t = tempfile::tempdir().unwrap();
    let parent = t.path().join("space ' $(touch INJECTED) ;");
    fs::create_dir(&parent).unwrap();
    let path = support::recipe_suite(&parent);
    let out = cli()
        .args(["compare", "--suite"])
        .arg(path)
        .arg("--headless")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let error = String::from_utf8(out.stderr).unwrap();
    let command = error
        .lines()
        .find_map(|line| line.strip_prefix("Recovery: "))
        .unwrap();
    let bin = t.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::copy(
        env!("CARGO_BIN_EXE_theseus"),
        bin.join(format!("theseus{}", std::env::consts::EXE_SUFFIX)),
    )
    .unwrap();
    let out = native::shell(command)
        .current_dir(t.path())
        .env("PATH", bin)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!t.path().join("INJECTED").exists());
}
