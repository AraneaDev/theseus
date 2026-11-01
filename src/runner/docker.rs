use theseus_host::process::NativeCommand as Command;
mod execution;
mod interruption;
mod mcp;
mod ownership;

use super::{
    artifacts::{self, Inputs},
    process, *,
};
use crate::{
    engine::{CancellationToken, now},
    recipe::Network,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

pub const OWNER_LABEL: &str = "io.theseus.owner";
pub const RUN_LABEL: &str = "io.theseus.run";
pub const SCENARIO_LABEL: &str = "io.theseus.scenario";

pub struct Docker {
    executable: std::ffi::OsString,
    inputs: Option<Inputs>,
    image_id: Option<String>,
    observed_roots: Vec<String>,
}
impl Default for Docker {
    fn default() -> Self {
        Self {
            executable: "docker".into(),
            inputs: None,
            image_id: None,
            observed_roots: Vec::new(),
        }
    }
}
impl Docker {
    /// Override the executable path (e.g. a configured Docker installation).
    pub fn new(executable: impl Into<std::ffi::OsString>) -> Self {
        Self {
            executable: executable.into(),
            ..Self::default()
        }
    }
    fn command(&self) -> Command {
        Command::new(&self.executable)
    }
    fn utility(&self, args: &[&str], cancel: &CancellationToken) -> Result<String> {
        process::utility(self.command().args(args), cancel)
    }
    fn root_exec(
        &self,
        resource: &OwnedResource,
        script: &str,
        args: &[&str],
        cancel: &CancellationToken,
    ) -> Result<String> {
        let mut command = self.command();
        command
            .args([
                "exec",
                "--user",
                "0:0",
                &resource.container_id,
                "/bin/sh",
                "-c",
                script,
                "theseus-setup",
            ])
            .args(args);
        process::utility(&mut command, cancel)
    }
    fn control(
        &mut self,
        resource: &OwnedResource,
        script: &str,
        args: &[&str],
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<String> {
        self.control_inner(resource, script, args, deadline, cancel, true)
    }
    #[allow(clippy::too_many_arguments)]
    fn control_inner(
        &mut self,
        resource: &OwnedResource,
        script: &str,
        args: &[&str],
        deadline: Instant,
        cancel: &CancellationToken,
        abort_guest: bool,
    ) -> Result<String> {
        let mut command = self.command();
        command
            .args([
                "exec",
                "--user",
                "0:0",
                &resource.container_id,
                "/bin/sh",
                "-c",
                script,
                "theseus-control",
            ])
            .args(args);
        let output = process::run(
            &mut command,
            deadline.saturating_duration_since(Instant::now()),
            cancel,
            None,
            512,
            || {
                if abort_guest {
                    self.stop(resource)
                } else {
                    Ok(())
                }
            },
        )?;
        ensure!(
            !output.cancelled && !output.timed_out && output.error.is_none(),
            "Docker completion control interrupted"
        );
        ensure!(
            output.status == Some(0),
            "Docker completion control failed (status {:?})",
            output.status
        );
        ensure!(
            !output.stdout.truncated,
            "application receipt exceeds limit"
        );
        Ok(String::from_utf8(output.stdout.bytes)?)
    }
    fn validate_resource(resource: &OwnedResource) -> Result<()> {
        ensure!(
            resource.container_id.len() == 64
                && resource.container_id.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid journaled container ID"
        );
        ensure!(
            resource.labels.get(OWNER_LABEL).map(String::as_str) == Some("v1")
                && resource.labels.contains_key(RUN_LABEL)
                && resource.labels.contains_key(SCENARIO_LABEL),
            "journal resource lacks ownership labels"
        );
        Ok(())
    }
    pub fn cleanup(&mut self, run: &Path) -> Result<()> {
        let mut journal = crate::report::load_journal(run)?;
        let directory = if run.is_dir() {
            run.to_path_buf()
        } else {
            run.parent().unwrap_or(Path::new(".")).to_path_buf()
        };
        // ReportStore only opens new directories; recovery persists journal with
        // the existing store's safe, private atomic-write boundary.
        let store = crate::report::open_existing(&directory)?;
        // Validate the whole recovery set before the first external acquisition:
        // no later validation return may discard an earlier native cleanup owner.
        for resource in &journal.resources {
            ensure!(
                resource.labels.get(RUN_LABEL) == Some(&journal.run_id),
                "journal run ownership mismatch"
            );
        }
        let mut errors = vec![];
        let mut native_cleanup = process::CleanupFailures::default();
        for resource in &mut journal.resources {
            if resource.removed {
                continue;
            }
            match self.remove(resource) {
                Ok(()) => resource.removed = true,
                Err(error) => {
                    native_cleanup.retain(&error);
                    errors.push(format!("{error:#}"));
                }
            }
        }
        journal.cleanup_errors = errors.clone();
        let saved = store.save_journal(&journal);
        native_cleanup.finish()?;
        saved?;
        ensure!(errors.is_empty(), "cleanup failed: {}", errors.join("; "));
        Ok(())
    }
    fn snapshot(
        &mut self,
        resource: &OwnedResource,
        root: &str,
        limits: Limits,
        cancel: &CancellationToken,
    ) -> Result<Snapshot> {
        // Check ancestors without following links; a root symlink itself is
        // legitimate evidence and docker cp (without -L) preserves it.
        let probe = r#"p=$1; while test "$p" != /; do p=${p%/*}; test -n "$p" || p=/; if test -L "$p"; then printf nofollow; exit 0; fi; done; if test -e "$1" || test -L "$1"; then printf present; else printf missing; fi"#;
        let present = self.root_exec(resource, probe, &[root], cancel)?;
        if present == "nofollow" {
            return Ok(Snapshot::incomplete(root, "symlink ancestor"));
        }
        if present == "missing" {
            return Ok(Snapshot::missing(root));
        }
        ensure!(present == "present", "unexpected root probe output");
        let mut command = self.command();
        command.args(["cp", &format!("{}:{root}", resource.container_id), "-"]);
        process::archive(
            &mut command,
            root,
            limits,
            Duration::from_secs(120),
            cancel,
            || {
                if cancel.is_cancelled() {
                    self.stop(resource)
                } else {
                    Ok(())
                }
            },
        )
    }
}
fn fresh_nonce() -> Result<String> {
    let mut bytes = [0; 16];
    theseus_host::random_bytes(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
impl Environment for Docker {
    fn prepare(
        &mut self,
        recipe: &ValidatedRecipe,
        pull: bool,
        cancel: &CancellationToken,
    ) -> Result<PreparedEnvironment> {
        self.utility(&["version", "--format", "{{.Server.Version}}"], cancel)
            .context("Docker daemon prerequisite; verify Docker is installed and reachable")?;
        ensure!(
            !recipe.recipe.image.starts_with('-'),
            "image reference cannot start with '-'"
        );
        let inspected = self.utility(
            &[
                "image",
                "inspect",
                "--format",
                "{{.Id}}",
                &recipe.recipe.image,
            ],
            cancel,
        );
        let image_id = match inspected {
            Ok(id) => id,
            Err(error) if pull => {
                let mut retained = process::CleanupFailures::default();
                retained.retain(&error);
                retained.finish()?;
                self.utility(&["pull", &recipe.recipe.image], cancel)?;
                self.utility(
                    &[
                        "image",
                        "inspect",
                        "--format",
                        "{{.Id}}",
                        &recipe.recipe.image,
                    ],
                    cancel,
                )
                .with_context(|| format!("inspect pulled image: {error:#}"))?
            }
            Err(error) => {
                return Err(error.context(format!(
                    "image {} is unavailable locally; use --pull to acquire it",
                    recipe.recipe.image
                )));
            }
        };
        let image_id = image_id.trim().to_string();
        ensure!(
            image_id.starts_with("sha256:")
                && image_id.len() == 71
                && image_id[7..].bytes().all(|b| b.is_ascii_hexdigit()),
            "Docker returned invalid image ID"
        );
        let volumes = self.utility(
            &[
                "image",
                "inspect",
                "--format",
                "{{json .Config.Volumes}}",
                &image_id,
            ],
            cancel,
        )?;
        let volumes: serde_json::Value =
            serde_json::from_str(&volumes).context("inspect image-declared volumes")?;
        ensure!(
            volumes.is_null() || volumes.as_object().is_some_and(|v| v.is_empty()),
            "image declares volumes; v1 requires an image without implicit mounts"
        );
        let inputs = artifacts::materialize(recipe, cancel)?;
        let artifacts = inputs.manifests.clone();
        self.inputs = Some(inputs);
        self.observed_roots = recipe.recipe.observe.clone();
        self.image_id = Some(image_id.clone());
        Ok(PreparedEnvironment {
            image_id,
            artifacts,
        })
    }
    fn create(
        &mut self,
        run_id: &str,
        scenario_id: &str,
        recipe: &ValidatedRecipe,
    ) -> Result<OwnedResource> {
        let labels = BTreeMap::from([
            (OWNER_LABEL.into(), "v1".into()),
            (RUN_LABEL.into(), run_id.into()),
            (SCENARIO_LABEL.into(), scenario_id.into()),
        ]);
        let mut command = self.command();
        command.args([
            "create",
            "--network",
            if recipe.recipe.network == Network::None {
                "none"
            } else {
                "bridge"
            },
            "--user",
            "0:0",
            "--entrypoint",
            "/bin/sh",
        ]);
        for (key, value) in &labels {
            command.args(["--label", &format!("{key}={value}")]);
        }
        command
            .arg(self.image_id.as_ref().context("image not prepared")?)
            .args(["-c", "while :; do sleep 3600; done"]);
        let id = process::utility(&mut command, &CancellationToken::default())?
            .trim()
            .to_string();
        let resource = OwnedResource {
            container_id: id,
            labels,
            removed: false,
        };
        Self::validate_resource(&resource)?;
        Ok(resource)
    }
    fn setup(
        &mut self,
        resource: &OwnedResource,
        _: &ValidatedRecipe,
        cancel: &CancellationToken,
    ) -> Result<()> {
        self.utility(&["start", &resource.container_id], cancel)?;
        self.root_exec(resource, r#"set -eu
for tool in /bin/sh /usr/bin/env sleep mkdir chown chmod tar cp rm cat id awk; do command -v "$tool" >/dev/null || { echo "required tool missing: $tool" >&2; exit 1; }; done
test "$(id -u)" = 0
awk -F: '$3==10001 && ($1!="theseus" || $4!=10001 || $6!="/home/theseus") {exit 1} $1=="theseus" && $3!=10001 {exit 1}' /etc/passwd || { echo 'incompatible UID 10001 / theseus account' >&2; exit 1; }
awk -F: '$3==10001 && $1!="theseus" {exit 1} $1=="theseus" && $3!=10001 {exit 1}' /etc/group || { echo 'incompatible GID 10001 / theseus group' >&2; exit 1; }
if ! awk -F: '$3==10001 {found=1} END {exit !found}' /etc/group; then printf 'theseus:x:10001:\n' >> /etc/group; fi
if ! awk -F: '$3==10001 {found=1} END {exit !found}' /etc/passwd; then printf 'theseus:x:10001:10001:Theseus:/home/theseus:/bin/sh\n' >> /etc/passwd; fi
for p in /home /home/theseus /home/theseus/.config /home/theseus/.local /home/theseus/.local/share /home/theseus/.local/bin /home/theseus/.cache /workspace /inputs; do test ! -L "$p" || { echo 'setup path is a symlink' >&2; exit 1; }; done
mkdir -p /home/theseus/.config /home/theseus/.local/share /home/theseus/.local/bin /home/theseus/.cache /workspace /inputs
chown -R 10001:10001 /home/theseus /workspace
chmod 755 /home/theseus /workspace /inputs
"#, &[], cancel)?;
        let inputs = self.inputs.as_ref().context("inputs not prepared")?;
        for manifest in &inputs.manifests {
            let source = inputs.directory.path().join(&manifest.name);
            let mut command = self.command();
            command.arg("cp").arg(&source).arg(format!(
                "{}:/inputs/{}",
                resource.container_id, manifest.name
            ));
            process::utility(&mut command, cancel)
                .context("copy materialized inputs into container")?;
        }
        #[cfg(windows)]
        self.root_exec(
            resource,
            artifacts::windows_input_normalization(),
            &[],
            cancel,
        )?;
        self.root_exec(
            resource,
            "test \"$(id -u theseus)\" = 10001 && test \"$(id -g theseus)\" = 10001",
            &[],
            cancel,
        )?;
        Ok(())
    }
    fn execute(
        &mut self,
        resource: &OwnedResource,
        request: CommandRequest,
        store: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        self.execute_ordinary(resource, request, store, cancel)
    }
    fn probe_mcp(
        &mut self,
        resource: &OwnedResource,
        request: CommandRequest,
        probe: crate::mcp::McpProbe,
        store: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<ExecutedMcp> {
        self.execute_mcp(resource, request, probe, store, cancel)
    }
    fn execute_interrupted(
        &mut self,
        resource: &OwnedResource,
        request: CommandRequest,
        store: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        self.interrupt(resource, request, store, cancel)
    }
    fn restart(
        &mut self,
        resource: &OwnedResource,
        cancel: &CancellationToken,
    ) -> Result<RestartEvidence> {
        self.restart_owned(resource, cancel)
    }
    fn observe(
        &mut self,
        resource: &OwnedResource,
        roots: &[String],
        limits: Limits,
        cancel: &CancellationToken,
    ) -> Result<Vec<Snapshot>> {
        Ok(self
            .observe_with_status(resource, roots, limits, cancel)?
            .snapshots)
    }
    fn observe_with_status(
        &mut self,
        resource: &OwnedResource,
        roots: &[String],
        limits: Limits,
        cancel: &CancellationToken,
    ) -> Result<ObservationBatch> {
        let mut batch = ObservationBatch {
            snapshots: vec![],
            infrastructure_error: None,
        };
        let mut native_cleanup = process::CleanupFailures::default();
        for root in roots {
            let snapshot = self
                .snapshot(resource, root, limits, cancel)
                .unwrap_or_else(|error| {
                    native_cleanup.retain(&error);
                    if error.downcast_ref::<process::ArchiveRejected>().is_none()
                        && batch.infrastructure_error.is_none()
                    {
                        batch.infrastructure_error = Some(format!("observe {root}: {error:#}"));
                    }
                    Snapshot::incomplete(root, format!("{error:#}"))
                });
            batch.snapshots.push(snapshot);
            if native_cleanup.pending() {
                break;
            }
        }
        native_cleanup.finish()?;
        Ok(batch)
    }
    fn stop(&mut self, resource: &OwnedResource) -> Result<()> {
        self.remove(resource)
    }
    fn remove(&mut self, resource: &OwnedResource) -> Result<()> {
        if self.verified_exists(resource)? {
            self.utility(
                &["rm", "-f", &resource.container_id],
                &CancellationToken::default(),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use process::test_support::docker as fixture;
    use std::{thread, time::Instant};
    fn probe_request(id: &str, expected_exit: i32) -> CommandRequest {
        CommandRequest {
            id: id.into(),
            phase: "check:probe".into(),
            identity: ExecutionIdentity::runtime(),
            argv: vec!["/bin/sh".into(), "-c".into(), "exit 1".into()],
            env: BTreeMap::new(),
            pass_env: vec![],
            timeout_seconds: 2,
            expected_exit,
            log_limit: 1024,
        }
    }
    fn probe_check(executed: &ExecutedCommand, expected_exit: i32) -> CheckOutcome {
        let check = crate::recipe::Check {
            id: "probe".into(),
            checkpoint: Checkpoint::Configured,
            expectation: crate::recipe::Expectation::Command {
                script: "exit 1".into(),
                timeout_seconds: 2,
                env: BTreeMap::new(),
                pass_env: vec![],
                expected_exit,
                stdout_contains: None,
            },
        };
        crate::checks::evaluate_command(
            &check,
            &crate::checks::CommandObservation {
                exit_status: executed.record.exit_status,
                stdout: executed.stdout.clone(),
                complete: executed.stdout_complete,
                error: executed.record.error.clone(),
            },
        )
        .outcome
    }
    #[test]
    fn json_capture_distinguishes_invalid_source_bytes_from_valid_replacement_character() {
        for interrupted in [false, true] {
            for (valid, truncated) in [(false, false), (true, false), (false, true)] {
                let temp = tempfile::tempdir().unwrap();
                let executable = fixture(
                    temp.path(),
                    if valid { "json-valid" } else { "json-invalid" },
                );
                let store = crate::report::create(&temp.path().join("run")).unwrap();
                let resource = OwnedResource {
                    container_id: "a".repeat(64),
                    labels: BTreeMap::from([
                        (OWNER_LABEL.into(), "v1".into()),
                        (RUN_LABEL.into(), "json".into()),
                        (SCENARIO_LABEL.into(), "capture".into()),
                    ]),
                    removed: false,
                };
                let mut docker = Docker::new(executable);
                let mut request = probe_request("json", 0);
                request.log_limit = if truncated {
                    2
                } else if valid {
                    5
                } else {
                    3
                };
                let cap = request.log_limit;
                let executed = if interrupted {
                    docker.execute_interrupted(
                        &resource,
                        request,
                        &store,
                        &CancellationToken::default(),
                    )
                } else {
                    docker.execute(&resource, request, &store, &CancellationToken::default())
                }
                .unwrap();
                assert_eq!(executed.stdout, if truncated { "\"�" } else { "\"�\"" });
                assert_eq!(executed.stdout_utf8, valid);
                assert_eq!(
                    executed.stdout_complete, !truncated,
                    "{:?}",
                    executed.record
                );
                let check:crate::recipe::Check=serde_json::from_value(serde_json::json!({"id":"json","checkpoint":"target_installed","kind":"json_command","script":"fixture","expected_exit":0,"max_stdout_bytes":cap,"assertions":[{"kind":"equals","pointer":"","equals_json":"\"�\""}]})).unwrap();
                let result = crate::checks::evaluate_json_command(
                    &check,
                    &crate::checks::JsonCommandObservation {
                        exit_status: executed.record.exit_status,
                        stdout: &executed.stdout,
                        complete: executed.stdout_complete,
                        utf8: executed.stdout_utf8,
                        error: executed.record.error.as_deref(),
                    },
                );
                assert_eq!(
                    result.outcome,
                    if truncated {
                        CheckOutcome::Incomplete
                    } else if valid {
                        CheckOutcome::Passed
                    } else {
                        CheckOutcome::Failed
                    }
                );
                let raw = std::fs::read(store.root().join(&executed.record.stdout.unwrap().path))
                    .unwrap();
                assert_eq!(std::str::from_utf8(&raw).is_ok(), valid);
            }
        }
    }
    #[test]
    fn docker_control_exit_one_cannot_satisfy_application_expectation() {
        let temp = tempfile::tempdir().unwrap();
        let executable = fixture(temp.path(), "failure");
        let store = crate::report::create(&temp.path().join("run")).unwrap();
        let resource = OwnedResource {
            container_id: "a".repeat(64),
            labels: BTreeMap::new(),
            removed: false,
        };
        for expected in [1, 0] {
            let result = Docker::new(executable.as_os_str())
                .execute(
                    &resource,
                    probe_request(&format!("infra-{expected}"), expected),
                    &store,
                    &CancellationToken::default(),
                )
                .unwrap();
            assert_eq!(probe_check(&result, expected), CheckOutcome::Incomplete);
            assert_eq!(result.record.exit_status, None);
            assert!(result.record.error.is_some());
        }
    }
    #[test]
    fn absent_stale_invalid_or_transport_failed_receipts_are_incomplete() {
        for mode in [
            "absent",
            "stale",
            "invalid",
            "overflow",
            "transport",
            "read-error",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let executable = fixture(temp.path(), &format!("receipt-{mode}"));
            let store = crate::report::create(&temp.path().join("run")).unwrap();
            let resource = OwnedResource {
                container_id: "a".repeat(64),
                labels: BTreeMap::new(),
                removed: false,
            };
            let result = Docker::new(executable.as_os_str())
                .execute(
                    &resource,
                    probe_request(mode, 1),
                    &store,
                    &CancellationToken::default(),
                )
                .unwrap();
            assert_eq!(probe_check(&result, 1), CheckOutcome::Incomplete, "{mode}");
            assert_eq!(result.record.exit_status, None, "{mode}");
            assert_eq!(
                result.stdout, "application stdout\n",
                "{mode}: {:?}",
                result.record.error
            );
            assert_eq!(
                std::fs::read_to_string(store.root().join(result.record.stderr.unwrap().path))
                    .unwrap(),
                "application stderr\n",
                "{mode}: {:?}",
                result.record.error
            );
        }
    }
    #[test]
    fn command_deadline_and_cancellation_cover_completion_control() {
        for phase in ["prepare", "receipt"] {
            for cancelled in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let marker = temp.path().join("control-started");
                let executable = fixture(temp.path(), &format!("wait-{phase}"));
                let cancel = CancellationToken::default();
                let trigger = cancel.clone();
                let signal = cancelled.then(|| {
                    thread::spawn(move || {
                        let deadline = Instant::now() + Duration::from_secs(2);
                        while !marker.exists() && Instant::now() < deadline {
                            thread::sleep(Duration::from_millis(5));
                        }
                        trigger.cancel();
                    })
                });
                let store = crate::report::create(&temp.path().join("run")).unwrap();
                let resource = OwnedResource {
                    container_id: "a".repeat(64),
                    labels: BTreeMap::from([
                        (OWNER_LABEL.into(), "v1".into()),
                        (RUN_LABEL.into(), "probe".into()),
                        (SCENARIO_LABEL.into(), "control".into()),
                    ]),
                    removed: false,
                };
                let mut request = probe_request("deadline", 1);
                request.timeout_seconds = 1;
                let started = Instant::now();
                let result = Docker::new(executable.as_os_str())
                    .execute(&resource, request, &store, &cancel)
                    .unwrap();
                if let Some(signal) = signal {
                    signal.join().unwrap();
                }
                assert!(started.elapsed() < Duration::from_secs(2), "{phase}");
                assert_eq!(result.record.exit_status, None);
                assert_eq!(result.record.cancelled, cancelled, "{phase}");
                assert_eq!(result.record.timed_out, !cancelled, "{phase}");
                assert_eq!(probe_check(&result, 1), CheckOutcome::Incomplete);
            }
        }
    }
    #[test]
    fn forwarded_default_values_are_not_claimed_in_serialized_evidence() {
        if std::env::var_os("THESEUS_METADATA_PROBE").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "runner::docker::tests::forwarded_default_values_are_not_claimed_in_serialized_evidence", "--nocapture"])
                .env("THESEUS_METADATA_PROBE", "1").env("PATH", "/synthetic/private-forwarded-path")
                .output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let executable = fixture(temp.path(), "argv");
        let store = crate::report::create(&temp.path().join("run")).unwrap();
        let resource = OwnedResource {
            container_id: "a".repeat(64),
            labels: BTreeMap::new(),
            removed: false,
        };
        let mut request = probe_request("env", 0);
        request.pass_env = vec!["PATH".into()];
        request
            .env
            .insert("PUBLIC_LITERAL".into(), "visible".into());
        let result = Docker::new(executable.as_os_str())
            .execute(&resource, request, &store, &CancellationToken::default())
            .unwrap();
        assert!(!result.record.environment.contains_key("PATH"));
        let serialized = serde_json::to_value(&result.record).unwrap();
        assert_eq!(
            serialized["forwarded_environment"],
            serde_json::json!(["PATH"])
        );
        assert_eq!(serialized["environment"]["PUBLIC_LITERAL"], "visible");
        let actual = std::env::var("PATH").unwrap();
        assert!(
            std::fs::read_to_string(temp.path().join("argv"))
                .unwrap()
                .contains(&format!("PATH={actual}"))
        );
        assert!(
            !serde_json::to_string(&result.record)
                .unwrap()
                .contains(&actual)
        );
        let mut old = serialized;
        old.as_object_mut().unwrap().remove("forwarded_environment");
        assert!(serde_json::from_value::<CommandRecord>(old).is_ok());
        let mut literal = probe_request("literal", 0);
        literal.env.insert("HOME".into(), "/public/home".into());
        let result = Docker::new(executable.as_os_str())
            .execute(&resource, literal, &store, &CancellationToken::default())
            .unwrap();
        assert_eq!(result.record.environment["HOME"], "/public/home");
        assert!(result.record.forwarded_environment.is_empty());
        assert!(
            serde_json::to_value(result.record)
                .unwrap()
                .get("forwarded_environment")
                .is_none()
        );
    }
    #[test]
    fn fix_round_one_mixed_journal_validation_precedes_all_native_child_acquisition() {
        let temp = tempfile::tempdir().unwrap();
        let executable = fixture(temp.path(), "argv");
        let store = crate::report::create(&temp.path().join("run")).unwrap();
        let resources = ["owned-run", "different-run"]
            .into_iter()
            .map(|run| OwnedResource {
                container_id: "a".repeat(64),
                labels: BTreeMap::from([
                    (OWNER_LABEL.into(), "v1".into()),
                    (RUN_LABEL.into(), run.into()),
                    (SCENARIO_LABEL.into(), "user-clean".into()),
                ]),
                removed: false,
            })
            .collect();
        store
            .save_journal(&RunJournal {
                schema_version: crate::model::JOURNAL_SCHEMA_VERSION,
                run_id: "owned-run".into(),
                resources,
                cleanup_errors: vec![],
            })
            .unwrap();
        assert_eq!(
            crate::report::load_journal(store.root())
                .unwrap()
                .resources
                .len(),
            2
        );
        let mut docker = Docker::new(executable);
        let mut journal = crate::report::load_journal(store.root()).unwrap();
        let mismatch = journal.resources.pop().unwrap();
        store.save_journal(&journal).unwrap();
        let control_error = docker.cleanup(store.root()).unwrap_err();
        assert!(
            temp.path().join("argv").exists(),
            "valid journal never acquired fixture: {control_error:#}"
        );
        std::fs::remove_file(temp.path().join("argv")).unwrap();
        journal.resources.push(mismatch);
        store.save_journal(&journal).unwrap();
        let error = docker.cleanup(store.root()).unwrap_err();
        assert!(format!("{error:#}").contains("journal run ownership mismatch"));
        assert!(
            !temp.path().join("argv").exists(),
            "mixed journal started a native removal/inspection before validating all run labels"
        );
    }

    #[test]
    fn archive_cancellation_still_applies_after_cli_exit_with_open_descriptors() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("bin")).unwrap();
        std::fs::write(temp.path().join("bin/sample"), b"sample").unwrap();
        let executable = fixture(temp.path(), "archive");
        let cancel = CancellationToken::default();
        let trigger = cancel.clone();
        let ready = temp.path().join("archive-peer-ready");
        let signal = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(700);
            while !ready.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            let established = ready.exists();
            // A bounded rescue cancels even on failed fixture setup, so the
            // supervisor still cleans its owned descendants before assertions.
            trigger.cancel();
            established
        });
        let began = Instant::now();
        let result = Docker::new(executable.as_os_str()).snapshot(
            &OwnedResource {
                container_id: "a".repeat(64),
                labels: BTreeMap::new(),
                removed: false,
            },
            "/usr/local/bin",
            Limits::default(),
            &cancel,
        );
        assert!(
            signal.join().unwrap(),
            "native archive leader-exit/held-stream prerequisite was not established: {result:?}"
        );
        assert!(
            result.is_err(),
            "archive incorrectly completed after {}ms",
            began.elapsed().as_millis()
        );
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains("cancelled"), "{error}");
        assert!(began.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn rejects_image_declared_volumes_before_creating_resources() {
        let temp = tempfile::tempdir().unwrap();
        let executable = fixture(temp.path(), "metadata");
        let recipe =
            crate::recipe::load(Path::new("tests/fixtures/lifecycle/recipe.toml")).unwrap();
        let mut docker = Docker::new(executable.as_os_str());
        let error = match docker.prepare(&recipe, false, &CancellationToken::default()) {
            Err(error) => error,
            Ok(_) => panic!("image-declared volumes were accepted"),
        };
        assert!(format!("{error:#}").contains("declares volumes"));
    }
}
