//! Synthetic UI fixtures, not evidence of a 70k-file lifecycle installation.
#[path = "support/browsing.rs"]
mod browsing;
#[allow(dead_code)]
#[path = "support/comparison.rs"]
mod comparison;
use std::{fs, path::Path};
use theseus::{comparison::ComparisonArtifact, model::*, report};

fn acceptance(case: &str, fixture: impl FnOnce(&Path) -> std::path::PathBuf) {
    let evidence = tempfile::Builder::new()
        .prefix(&format!("theseus-browsing-task4-{case}-"))
        .tempdir()
        .unwrap()
        .keep();
    let root = fixture(&evidence);
    fs::write(evidence.join("SYNTHETIC-UI-FIXTURE.txt"),
        "Synthetic saved evidence for public CLI browsing acceptance only. No installation was executed.\n").unwrap();
    println!("Synthetic UI evidence: {}", evidence.display());
    let output = std::process::Command::new(if cfg!(windows) { "python" } else { "python3" })
        .args([
            "tests/support/browsing_pty.py",
            env!("CARGO_BIN_EXE_theseus"),
            case,
        ])
        .arg(&root)
        .arg(&evidence)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    fs::write(evidence.join("acceptance.stdout"), &output.stdout).unwrap();
    fs::write(evidence.join("acceptance.stderr"), &output.stderr).unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn public_report_pty_reaches_cache_overflow_unicode_details_and_typed_log_tail() {
    acceptance("report", |evidence| {
        let root = evidence.join("synthetic run's evidence");
        let store = report::create(&root).unwrap();
        let mut saved = browsing::metadata_report(70_000, 40_001);
        let path = format!(
            "logs/{}/PATH_TAIL",
            (0..20)
                .map(|i| format!("{i:02}-{}", "p".repeat(35)))
                .collect::<Vec<_>>()
                .join("/")
        );
        // Scalar straddles the 4096-byte raw boundary. Newlines keep it visible
        // after bounded local scrolling; later data extends beyond the old 64KiB.
        let mut bytes = b"STDOUT_ONLY\n".to_vec();
        bytes.extend(vec![b'a'; 4095 - bytes.len()]);
        bytes.extend("🙂\nSECOND_CHUNK\n".as_bytes());
        bytes.extend(vec![b'b'; 80_000]);
        bytes.extend(b"\nLOG_TAIL");
        fs::create_dir_all(root.join(&path).parent().unwrap()).unwrap();
        store.write_log(&path, &bytes, bytes.len()).unwrap();
        let stderr = format!("{path} (stderr)");
        store.write_log(&stderr, b"STDERR_ONLY", 100).unwrap();
        let mut cmd = CommandRecord::new("synthetic cmd", "UI fixture only");
        cmd.stdout = Some(LogReference {
            path,
            bytes: bytes.len() as u64,
            truncated: true,
        });
        cmd.stderr = Some(LogReference {
            path: stderr,
            bytes: 11,
            truncated: false,
        });
        saved.scenarios[0].stages[0].commands.push(cmd);
        store.save(&saved).unwrap();
        root
    });
}

#[test]
fn public_comparison_pty_recomputes_and_reaches_difference_101_and_source_detail() {
    acceptance("comparison", |evidence| {
        let root = comparison::saved(evidence);
        let artifact_path = root.join("comparison.json");
        let mut artifact: ComparisonArtifact =
            serde_json::from_slice(&fs::read(&artifact_path).unwrap()).unwrap();
        for source in &mut artifact.sources {
            let source_root = root.join(&source.path);
            let store = report::open_existing(&source_root).unwrap();
            let mut saved = browsing::metadata_report(
                if source.variant_id == "archive" {
                    101
                } else {
                    0
                },
                40_001,
            );
            saved.recipe_digest = "a".repeat(64);
            saved.image = format!("{}SOURCE_TAIL", "x".repeat(1000));
            saved.image_id = Some(format!("sha256:{}", source.variant_id));
            saved.run_id = format!("run-{}", source.variant_id);
            store.save(&saved).unwrap();
            source.report_digest =
                Some(comparison::digest(&fs::read(store.report_path()).unwrap()));
        }
        fs::write(&artifact_path, serde_json::to_vec(&artifact).unwrap()).unwrap();
        root
    });
}
