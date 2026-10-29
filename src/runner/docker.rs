use super::{
    artifacts::{self, Inputs},
    process, *,
};
use crate::{
    engine::{CancellationToken, now},
    recipe::Network,
};
use anyhow::{Context, Result, bail, ensure};
use std::{collections::BTreeMap, path::Path, process::Command, time::Duration};

pub const OWNER_LABEL: &str = "io.theseus.owner";
pub const RUN_LABEL: &str = "io.theseus.run";
pub const SCENARIO_LABEL: &str = "io.theseus.scenario";

pub struct Docker {
    executable: std::ffi::OsString,
    inputs: Option<Inputs>,
    image_id: Option<String>,
}
impl Default for Docker {
    fn default() -> Self {
        Self {
            executable: "docker".into(),
            inputs: None,
            image_id: None,
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
    fn verified_exists(&self, resource: &OwnedResource) -> Result<bool> {
        Self::validate_resource(resource)?;
        let output = process::run(
            self.command().args([
                "container",
                "inspect",
                "--format",
                "{{json .}}",
                &resource.container_id,
            ]),
            Duration::from_secs(30),
            &CancellationToken::default(),
            None,
            1_048_576,
            || Ok(()),
        )?;
        ensure!(
            !output.timed_out && !output.cancelled,
            "ownership inspection timed out"
        );
        ensure!(
            output.error.is_none(),
            "ownership inspection failed: {:?}",
            output.error
        );
        if output.status != Some(0) {
            let error = String::from_utf8_lossy(&output.stderr.bytes);
            if error.contains("No such container:") || error.contains("No such object:") {
                return Ok(false);
            }
            bail!("ownership inspection failed: {error}");
        }
        ensure!(!output.stdout.truncated, "ownership metadata exceeds limit");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
        ensure!(
            value["Id"].as_str() == Some(&resource.container_id),
            "container ID mismatch"
        );
        for (key, expected) in &resource.labels {
            ensure!(
                value["Config"]["Labels"][key].as_str() == Some(expected),
                "container ownership label mismatch: {key}"
            );
        }
        Ok(true)
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
        let mut errors = vec![];
        for resource in &mut journal.resources {
            ensure!(
                resource.labels.get(RUN_LABEL) == Some(&journal.run_id),
                "journal run ownership mismatch"
            );
            if resource.removed {
                continue;
            }
            match self.remove(resource) {
                Ok(()) => resource.removed = true,
                Err(error) => errors.push(format!("{error:#}")),
            }
        }
        journal.cleanup_errors = errors.clone();
        store.save_journal(&journal)?;
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
        let probe = r#"p=$1; while test "$p" != /; do p=${p%/*}; test -n "$p" || p=/; if test -L "$p"; then echo 'symlink ancestor' >&2; exit 4; fi; done; if test -e "$1" || test -L "$1"; then printf present; else printf missing; fi"#;
        let present = self.root_exec(resource, probe, &[root], cancel)?;
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
            Err(error) => bail!(
                "image {} is unavailable locally; use --pull to acquire it: {error:#}",
                recipe.recipe.image
            ),
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
        let mut public = BTreeMap::from([
            ("HOME".into(), request.identity.home.clone()),
            (
                "XDG_CONFIG_HOME".into(),
                format!("{}/.config", request.identity.home),
            ),
            (
                "XDG_DATA_HOME".into(),
                format!("{}/.local/share", request.identity.home),
            ),
            (
                "XDG_CACHE_HOME".into(),
                format!("{}/.cache", request.identity.home),
            ),
            (
                "PATH".into(),
                "/home/theseus/.local/bin:/usr/local/bin:/usr/bin:/bin".into(),
            ),
            (
                "USER".into(),
                if request.identity.uid == 0 {
                    "root"
                } else {
                    "theseus"
                }
                .into(),
            ),
            (
                "LOGNAME".into(),
                if request.identity.uid == 0 {
                    "root"
                } else {
                    "theseus"
                }
                .into(),
            ),
        ]);
        public.extend(request.env.clone());
        let mut actual: BTreeMap<std::ffi::OsString, std::ffi::OsString> =
            public.iter().map(|(k, v)| (k.into(), v.into())).collect();
        for name in &request.pass_env {
            actual.insert(
                name.into(),
                std::env::var_os(name)
                    .with_context(|| format!("missing pass_env variable {name}"))?,
            );
        }
        let stdout_path = format!("logs/{}-stdout.log", request.id);
        let stderr_path = format!("logs/{}-stderr.log", request.id);
        let files = (
            store.create_log(&stdout_path)?,
            store.create_log(&stderr_path)?,
        );
        let mut command = self.command();
        command.args([
            "exec",
            "--user",
            &format!("{}:{}", request.identity.uid, request.identity.gid),
            "--workdir",
            "/workspace",
            &resource.container_id,
            "/usr/bin/env",
            "-i",
        ]);
        for (key, value) in actual {
            let mut pair = key;
            pair.push("=");
            pair.push(value);
            command.arg(pair);
        }
        command.args(&request.argv);
        let mut record = CommandRecord::new(&request.id, &request.phase);
        record.identity = request.identity;
        record.environment = public;
        record.argv = request.argv;
        record.started_at = now();
        let output = process::run(
            &mut command,
            Duration::from_secs(request.timeout_seconds),
            cancel,
            Some(files),
            request.log_limit.min(crate::recipe::DEFAULT_LOG_LIMIT) as usize,
            || self.stop(resource),
        )?;
        record.error = output.error;
        record.duration_ms = output.duration_ms;
        record.exit_status = output.status;
        record.timed_out = output.timed_out;
        record.cancelled = output.cancelled;
        record.skip_reason = None;
        record.outcome = if output.cancelled {
            CommandOutcome::Cancelled
        } else if output.timed_out {
            CommandOutcome::TimedOut
        } else if output.status == Some(request.expected_exit) {
            CommandOutcome::Succeeded
        } else {
            CommandOutcome::Failed
        };
        record.stdout = Some(LogReference {
            path: stdout_path,
            bytes: output.stdout.stored,
            truncated: output.stdout.truncated,
        });
        record.stderr = Some(LogReference {
            path: stderr_path,
            bytes: output.stderr.stored,
            truncated: output.stderr.truncated,
        });
        Ok(ExecutedCommand {
            record,
            stdout: String::from_utf8_lossy(&output.stdout.bytes).into_owned(),
            stdout_complete: !output.stdout.truncated,
        })
    }
    fn observe(
        &mut self,
        resource: &OwnedResource,
        roots: &[String],
        limits: Limits,
        cancel: &CancellationToken,
    ) -> Result<Vec<Snapshot>> {
        Ok(roots
            .iter()
            .map(|root| {
                self.snapshot(resource, root, limits, cancel)
                    .unwrap_or_else(|error| Snapshot::incomplete(root, format!("{error:#}")))
            })
            .collect())
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
    use std::{os::unix::fs::PermissionsExt, thread, time::Instant};
    #[test]
    fn archive_cancellation_still_applies_after_cli_exit_with_open_descriptors() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("bin")).unwrap();
        std::fs::write(temp.path().join("bin/sample"), b"sample").unwrap();
        let executable = temp.path().join("docker-wrapper");
        std::fs::write(&executable, format!("#!/bin/sh\ncase \"$1\" in\nexec) printf present ;;\ncp) tar -C '{}' -cf - bin; sleep 2 & exit 0 ;;\n*) exit 99 ;;\nesac\n", temp.path().display())).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cancel = CancellationToken::default();
        let trigger = cancel.clone();
        let signal = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            trigger.cancel();
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
        signal.join().unwrap();
        assert!(
            result.is_err(),
            "archive incorrectly completed after {}ms",
            began.elapsed().as_millis()
        );
        assert!(format!("{:#}", result.unwrap_err()).contains("cancelled"));
        assert!(began.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn rejects_image_declared_volumes_before_creating_resources() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("docker-metadata");
        std::fs::write(&executable, "#!/bin/sh\ncase \"$1:$2\" in\nversion:--format) printf '29.9.2\\n' ;;\nimage:inspect) case \"$4\" in\n'{{.Id}}') printf 'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\n' ;;\n'{{json .Config.Volumes}}') printf '{\"/data\":{}}\\n' ;;\nesac ;;\n*) echo 'unexpected mutation' >&2; exit 99 ;;\nesac\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
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
