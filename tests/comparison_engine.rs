#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[path = "support/native.rs"]
mod native;
use anyhow::Result;
use native::symlink;
use std::{collections::BTreeMap, fs};
use theseus::{
    comparison::{self, ComparisonStatus, SuiteRunOptions},
    engine::{CancellationToken, EventKind},
    model::*,
    recipe::*,
    report::ReportStore,
    runner::*,
    snapshot::{FileEntry, ObservationStatus, Snapshot},
};

#[derive(Default)]
struct Fake {
    prepares: usize,
    creates: usize,
    removes: usize,
    difference: bool,
    fail_first: bool,
    infrastructure: bool,
    setup_failure: bool,
    incomplete: bool,
    cleanup_failure: bool,
    observe_error: bool,
    execute_error: bool,
    require_reference: Option<std::path::PathBuf>,
    swap: Option<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)>,
}
impl Environment for Fake {
    fn prepare(
        &mut self,
        _: &ValidatedRecipe,
        _: bool,
        _: &CancellationToken,
    ) -> Result<PreparedEnvironment> {
        self.prepares += 1;
        if let Some(output) = &self.require_reference {
            let suite = fs::read_dir(output)?.next().unwrap()?.path();
            let artifact: serde_json::Value =
                serde_json::from_slice(&fs::read(suite.join("comparison.json"))?)?;
            assert_eq!(artifact["sources"].as_array().unwrap().len(), self.prepares);
            let source = &artifact["sources"][self.prepares - 1];
            assert!(source["report_digest"].is_null());
            assert!(
                suite
                    .join(source["path"].as_str().unwrap())
                    .join("journal.json")
                    .is_file()
            );
        }
        if self.infrastructure {
            anyhow::bail!("daemon unavailable");
        }
        Ok(PreparedEnvironment {
            image_id: format!("sha256:image{}", self.prepares),
            artifacts: vec![],
        })
    }
    fn create(&mut self, run: &str, scenario: &str, _: &ValidatedRecipe) -> Result<OwnedResource> {
        self.creates += 1;
        Ok(OwnedResource {
            container_id: "a".repeat(64),
            labels: BTreeMap::from([
                ("io.theseus.owner".into(), "v1".into()),
                ("io.theseus.run".into(), run.into()),
                ("io.theseus.scenario".into(), scenario.into()),
            ]),
            removed: false,
        })
    }
    fn setup(
        &mut self,
        _: &OwnedResource,
        _: &ValidatedRecipe,
        _: &CancellationToken,
    ) -> Result<()> {
        if self.setup_failure {
            anyhow::bail!("setup transport failed");
        }
        Ok(())
    }
    fn execute(
        &mut self,
        _: &OwnedResource,
        request: CommandRequest,
        _: &ReportStore,
        _: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        if self.execute_error {
            anyhow::bail!("command transport failed");
        }
        let mut record = CommandRecord::new(&request.id, &request.phase);
        record.identity = request.identity;
        record.argv = request.argv;
        let failed = self.fail_first && self.prepares == 1 && request.phase.starts_with("install");
        record.exit_status = Some(if failed { 1 } else { 0 });
        record.outcome = if failed {
            CommandOutcome::Failed
        } else {
            CommandOutcome::Succeeded
        };
        Ok(ExecutedCommand {
            record,
            stdout: String::new(),
            stdout_utf8: true,
            stdout_complete: true,
        })
    }
    fn observe(
        &mut self,
        _: &OwnedResource,
        roots: &[String],
        _: Limits,
        _: &CancellationToken,
    ) -> Result<Vec<Snapshot>> {
        if self.observe_error {
            anyhow::bail!("observation transport failed");
        }
        if self.incomplete {
            return Ok(roots
                .iter()
                .map(|r| Snapshot::incomplete(r, "observation limit"))
                .collect());
        }
        Ok(roots
            .iter()
            .map(|root| Snapshot {
                root: root.clone(),
                completeness: ObservationStatus::Complete,
                entries: BTreeMap::from([(
                    root.clone(),
                    FileEntry {
                        kind: FileKind::Directory,
                        mode: 0o755,
                        uid: if self.difference && self.prepares == 2 {
                            2
                        } else {
                            1
                        },
                        gid: 1,
                        size: 0,
                        sha256: None,
                        link_target: None,
                    },
                )]),
            })
            .collect())
    }
    fn stop(&mut self, _: &OwnedResource) -> Result<()> {
        Ok(())
    }
    fn remove(&mut self, _: &OwnedResource) -> Result<()> {
        self.removes += 1;
        if self.cleanup_failure {
            anyhow::bail!("cleanup transport failed");
        }
        if let Some((root, moved, outside)) = self.swap.take() {
            fs::rename(&root, moved)?;
            symlink(outside, root)?;
        }
        Ok(())
    }
}
fn fixture() -> (
    tempfile::TempDir,
    comparison::ValidatedSuite,
    SuiteRunOptions,
) {
    let t = tempfile::tempdir().unwrap();
    fs::create_dir(t.path().join("target")).unwrap();
    fs::write(
        t.path().join("recipe.toml"),
        include_str!("fixtures/recipes/v2_clean.toml"),
    )
    .unwrap();
    fs::write(t.path().join("comparison.toml"), "schema_version=1\nname='methods'\nbaseline='copy'\nprofiles=['user']\nscenarios=['clean']\nstages=['configured']\n[[variants]]\nid='copy'\nrecipe='recipe.toml'\n[[variants]]\nid='archive'\nrecipe='recipe.toml'\n[[scopes]]\nid='home'\nprofile='user'\npath='/home/theseus'\n").unwrap();
    let suite = comparison::load(&t.path().join("comparison.toml")).unwrap();
    let options = SuiteRunOptions {
        output_root: t.path().join("runs"),
        pull: true,
    };
    (t, suite, options)
}
#[test]
fn shared_engine_runs_fresh_sources_and_persists_private_portable_evidence() {
    for difference in [false, true] {
        let (t, suite, options) = fixture();
        let mut fake = Fake {
            difference,
            require_reference: Some(options.output_root.clone()),
            ..Fake::default()
        };
        let result = comparison::run_with(
            &suite,
            options,
            CancellationToken::default(),
            &mut fake,
            |_| {},
        )
        .unwrap();
        assert_eq!(fake.creates, 2);
        assert_eq!(fake.removes, 2);
        assert_eq!(
            result.report.outcome,
            if difference {
                RunOutcome::Failed
            } else {
                RunOutcome::Passed
            }
        );
        assert_eq!(
            result.report.cells[0].comparison.status,
            if difference {
                ComparisonStatus::Different
            } else {
                ComparisonStatus::Equivalent
            }
        );
        assert_eq!(result.report.sources.len(), 2);
        assert_eq!(
            result.report.sources[0].image_id.as_deref(),
            Some("sha256:image1")
        );
        assert_eq!(
            result.report.sources[1].image_id.as_deref(),
            Some("sha256:image2")
        );
        assert!(
            result.report.sources[0]
                .report_path
                .as_ref()
                .unwrap()
                .starts_with("sources/copy/")
        );
        // POSIX mode-bit introspection; Windows private DACL is covered by native host tests.
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&result.report_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(result.report_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&result.report_path).unwrap()).unwrap();
        for source in value["sources"].as_array().unwrap() {
            let source_dir = result
                .report_path
                .parent()
                .unwrap()
                .join(source["path"].as_str().unwrap());
            #[cfg(unix)]
            assert_eq!(
                fs::metadata(&source_dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            let r = theseus::report::load(&source_dir).unwrap();
            assert!(r.image_id.unwrap().starts_with("sha256:image"));
            assert_eq!(r.recipe_digest, suite.variants[0].recipe.digest);
        }
        fs::remove_file(t.path().join("recipe.toml")).unwrap();
        fs::remove_dir(t.path().join("target")).unwrap();
        let relocated = t.path().join("relocated");
        fs::rename(result.report_path.parent().unwrap(), &relocated).unwrap();
        assert_eq!(
            comparison::load_report(&relocated).unwrap().outcome,
            result.report.outcome
        );
    }
}
#[test]
fn failed_lifecycle_continues_but_infrastructure_failure_stops_variants() {
    let (_t, suite, options) = fixture();
    let mut fake = Fake {
        fail_first: true,
        ..Fake::default()
    };
    let r = comparison::run_with(
        &suite,
        options.clone(),
        CancellationToken::default(),
        &mut fake,
        |_| {},
    )
    .unwrap();
    assert_eq!(fake.creates, 2);
    assert_eq!(r.report.outcome, RunOutcome::Failed);
    let mut fake = Fake {
        infrastructure: true,
        ..Fake::default()
    };
    let r = comparison::run_with(
        &suite,
        options,
        CancellationToken::default(),
        &mut fake,
        |_| {},
    )
    .unwrap();
    assert_eq!(fake.prepares, 1);
    assert_eq!(fake.creates, 0);
    assert_eq!(r.report.outcome, RunOutcome::Incomplete);
}
#[test]
fn cancellation_before_first_and_between_variants_survives_reload() {
    for early in [true, false] {
        let (_t, suite, options) = fixture();
        let mut fake = Fake::default();
        let cancel = CancellationToken::default();
        if early {
            cancel.cancel();
        }
        let r = comparison::run_with(&suite, options, cancel.clone(), &mut fake, |event| {
            if event
                .source
                .as_ref()
                .is_some_and(|e| e.kind == EventKind::Finished)
            {
                cancel.cancel();
            }
        })
        .unwrap();
        assert_eq!(fake.creates, usize::from(!early));
        assert_eq!(r.report.outcome, RunOutcome::Cancelled);
        assert_eq!(
            comparison::load_report(&r.report_path).unwrap().outcome,
            RunOutcome::Cancelled
        );
    }
}
#[test]
fn source_reference_save_failure_at_started_prevents_even_prepare_and_exposes_recovery() {
    let (_t, suite, options) = fixture();
    let mut fake = Fake::default();
    let mut path = None;
    let result = comparison::run_with(
        &suite,
        options,
        CancellationToken::default(),
        &mut fake,
        |event| {
            if event
                .source
                .as_ref()
                .is_some_and(|e| e.kind == EventKind::Started)
            {
                path = Some(event.report_path.clone());
                fs::remove_file(&event.report_path).unwrap();
                fs::create_dir(&event.report_path).unwrap();
            }
        },
    );
    let error = match result {
        Ok(_) => panic!("reference save failure was swallowed"),
        Err(e) => format!("{e:#}"),
    };
    assert_eq!(fake.creates, 0);
    assert_eq!(fake.prepares, 0);
    assert!(error.contains(path.unwrap().to_str().unwrap()));
    assert!(error.contains(&format!(
        "sources{}copy{}",
        std::path::MAIN_SEPARATOR,
        std::path::MAIN_SEPARATOR
    )));
}
#[test]
fn output_symlink_parent_is_rejected_before_mutation() {
    let (t, suite, mut options) = fixture();
    fs::create_dir(t.path().join("outside")).unwrap();
    symlink(t.path().join("outside"), &options.output_root).unwrap();
    options.output_root = options.output_root.join("nested");
    let mut fake = Fake::default();
    assert!(
        comparison::run_with(
            &suite,
            options,
            CancellationToken::default(),
            &mut fake,
            |_| {}
        )
        .is_err()
    );
    assert_eq!(fake.prepares, 0);
    assert!(!t.path().join("outside/nested").exists());
}
#[test]
fn setup_infrastructure_failure_stops_but_incomplete_observation_continues() {
    let (_t, suite, options) = fixture();
    let mut fake = Fake {
        setup_failure: true,
        ..Fake::default()
    };
    let r = comparison::run_with(
        &suite,
        options.clone(),
        CancellationToken::default(),
        &mut fake,
        |_| {},
    )
    .unwrap();
    assert_eq!(fake.creates, 1);
    assert_eq!(r.report.outcome, RunOutcome::Incomplete);
    assert!(
        r.report.sources[0]
            .error
            .as_ref()
            .unwrap()
            .contains("setup transport failed")
    );
    let mut fake = Fake {
        incomplete: true,
        ..Fake::default()
    };
    let r = comparison::run_with(
        &suite,
        options,
        CancellationToken::default(),
        &mut fake,
        |_| {},
    )
    .unwrap();
    assert_eq!(fake.creates, 2);
    assert_eq!(r.report.outcome, RunOutcome::Incomplete);
}
#[test]
fn cleanup_infrastructure_failure_stops_new_variants() {
    let (_t, suite, options) = fixture();
    let mut fake = Fake {
        cleanup_failure: true,
        ..Fake::default()
    };
    let r = comparison::run_with(
        &suite,
        options,
        CancellationToken::default(),
        &mut fake,
        |_| {},
    )
    .unwrap();
    assert_eq!(fake.creates, 1);
    assert_eq!(r.report.outcome, RunOutcome::Incomplete);
    assert!(
        r.report.sources[0]
            .error
            .as_ref()
            .unwrap()
            .contains("cleanup transport failed")
    );
}
#[test]
fn boundary_errors_remain_infrastructure_errors_after_engine_normalizes_them() {
    for observe_error in [true, false] {
        let (_t, suite, options) = fixture();
        let mut fake = Fake {
            observe_error,
            execute_error: !observe_error,
            ..Fake::default()
        };
        let r = comparison::run_with(
            &suite,
            options,
            CancellationToken::default(),
            &mut fake,
            |_| {},
        )
        .unwrap();
        assert_eq!(fake.creates, 1);
        assert_eq!(r.report.outcome, RunOutcome::Incomplete);
        assert!(
            r.report.sources[0]
                .error
                .as_ref()
                .unwrap()
                .contains("transport failed")
        );
    }
}
#[test]
fn unfinished_cleanup_uses_pinned_journals_and_validates_all_ownership_before_removal() {
    let (t, suite, options) = fixture();
    let mut fake = Fake::default();
    let r = comparison::run_with(
        &suite,
        options,
        CancellationToken::default(),
        &mut fake,
        |_| {},
    )
    .unwrap();
    let mut artifact: serde_json::Value =
        serde_json::from_slice(&fs::read(&r.report_path).unwrap()).unwrap();
    artifact["finished"] = false.into();
    let root = r.report_path.parent().unwrap();
    let mut source_dirs = vec![];
    for source in artifact["sources"].as_array_mut().unwrap() {
        source["report_digest"] = serde_json::Value::Null;
        let source_dir = root.join(source["path"].as_str().unwrap());
        fs::remove_file(source_dir.join("report.json")).unwrap();
        let mut journal = theseus::report::load_journal(&source_dir).unwrap();
        journal.resources[0].removed = false;
        theseus::report::open_existing(&source_dir)
            .unwrap()
            .save_journal(&journal)
            .unwrap();
        source_dirs.push(source_dir);
    }
    fs::write(&r.report_path, serde_json::to_vec(&artifact).unwrap()).unwrap();
    assert_eq!(
        comparison::load_report(&r.report_path).unwrap().outcome,
        RunOutcome::Incomplete
    );
    let mut journal = theseus::report::load_journal(&source_dirs[1]).unwrap();
    let original = journal.resources[0].labels["io.theseus.run"].clone();
    journal.resources[0]
        .labels
        .insert("io.theseus.run".into(), "forged".into());
    let second = theseus::report::open_existing(&source_dirs[1]).unwrap();
    second.save_journal(&journal).unwrap();
    let mut cleaner = Fake::default();
    assert!(comparison::cleanup_with(root, &mut cleaner).is_err());
    assert_eq!(cleaner.removes, 0);
    journal.resources[0]
        .labels
        .insert("io.theseus.run".into(), original);
    second.save_journal(&journal).unwrap();
    let moved = t.path().join("moved-source");
    let outside = t.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("journal.json"), b"untouched").unwrap();
    cleaner.swap = Some((source_dirs[0].clone(), moved.clone(), outside.clone()));
    comparison::cleanup_with(root, &mut cleaner).unwrap();
    assert_eq!(cleaner.removes, 2);
    assert_eq!(
        fs::read(outside.join("journal.json")).unwrap(),
        b"untouched"
    );
    assert!(theseus::report::load_journal(&moved).unwrap().resources[0].removed);
    assert!(
        theseus::report::load_journal(&source_dirs[1])
            .unwrap()
            .resources[0]
            .removed
    );
}

// This executable replaces only the Docker CLI transport. Preparation, artifact
// materialization, observation parsing, lifecycle execution and ownership checks
// all use the production Docker adapter and engine.
fn docker_transport(directory: &std::path::Path, mode: &str) -> Docker {
    Docker::new(native::docker(directory, &format!("comparison-{mode}")))
}

fn docker_calls(directory: &std::path::Path, operation: &str) -> usize {
    fs::read_to_string(directory.join("docker-calls.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Vec<String>>(line).unwrap())
        .filter(|args| args[0] == operation)
        .count()
}
#[test]
fn production_docker_transport_failures_stop_next_variant_after_successful_teardown() {
    for mode in [
        "probe-transport",
        "archive-transport",
        "archive-io",
        "command-transport",
    ] {
        let (t, suite, options) = fixture();
        let r = comparison::run_with(
            &suite,
            options,
            CancellationToken::default(),
            &mut docker_transport(t.path(), mode),
            |_| {},
        )
        .unwrap();
        assert_eq!(
            docker_calls(t.path(), "version"),
            1,
            "next variant prepared after {mode}"
        );
        assert_eq!(
            docker_calls(t.path(), "rm"),
            1,
            "owned teardown must succeed after {mode}"
        );
        assert_eq!(r.report.outcome, RunOutcome::Incomplete);
        assert!(r.report.sources[0].error.is_some(), "{mode}");
        let source = r
            .report_path
            .parent()
            .unwrap()
            .join(r.report.sources[0].report_path.as_ref().unwrap());
        let report = theseus::report::load(&source).unwrap();
        assert!(matches!(
            report.scenarios[0].teardown,
            TeardownStatus::Complete
        ));
        let journal = theseus::report::load_journal(source.parent().unwrap()).unwrap();
        assert!(journal.resources[0].removed);
    }
}
#[test]
fn production_docker_bounded_and_nofollow_observations_continue_variants() {
    for mode in ["limit", "nofollow"] {
        let (t, mut suite, options) = fixture();
        for variant in &mut suite.variants {
            variant.recipe.recipe.limits.max_entries = 1;
        }
        let r = comparison::run_with(
            &suite,
            options,
            CancellationToken::default(),
            &mut docker_transport(t.path(), mode),
            |_| {},
        )
        .unwrap();
        assert_eq!(docker_calls(t.path(), "version"), 2, "{mode}");
        assert_eq!(docker_calls(t.path(), "rm"), 2, "{mode}");
        assert_eq!(r.report.outcome, RunOutcome::Incomplete);
        assert!(
            r.report.sources.iter().all(|source| source.error.is_none()),
            "{mode}"
        );
    }
}
#[test]
fn ordinary_docker_observe_preserves_incomplete_snapshots_for_transport_errors() {
    let t = tempfile::tempdir().unwrap();
    let mut docker = docker_transport(t.path(), "probe-transport");
    let resource = OwnedResource {
        container_id: "a".repeat(64),
        labels: BTreeMap::new(),
        removed: false,
    };
    let snapshots = docker
        .observe(
            &resource,
            &["/home/theseus".into()],
            Limits::default(),
            &CancellationToken::default(),
        )
        .unwrap();
    assert!(matches!(
        snapshots[0].completeness,
        ObservationStatus::Incomplete { .. }
    ));
}
