//! Shared command preparation and authenticated application completion.
use super::*;
use std::{ffi::OsString, fs::File};

pub(super) struct Launch {
    pub(super) files: Option<(File, File)>,
    pub(super) stdout_path: String,
    pub(super) stderr_path: String,
    pub(super) started: Instant,
    pub(super) deadline: Instant,
    pub(super) directory: String,
    pub(super) nonce: String,
    actual: BTreeMap<OsString, OsString>,
    pub(super) prepared: Result<()>,
}

impl Launch {
    pub(super) fn receipt(&self) -> String {
        format!("{}/command/completion", self.directory)
    }
}

impl Docker {
    pub(super) fn prepare_execution(
        &mut self,
        resource: &OwnedResource,
        request: &CommandRequest,
        store: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<(CommandRecord, Launch)> {
        self.prepare_execution_inner(resource, request, store, cancel, false)
    }

    pub(super) fn prepare_interrupted_execution(
        &mut self,
        resource: &OwnedResource,
        request: &CommandRequest,
        store: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<(CommandRecord, Launch)> {
        self.prepare_execution_inner(resource, request, store, cancel, true)
    }

    fn prepare_execution_inner(
        &mut self,
        resource: &OwnedResource,
        request: &CommandRequest,
        store: &ReportStore,
        cancel: &CancellationToken,
        interrupted: bool,
    ) -> Result<(CommandRecord, Launch)> {
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
            public.remove(name);
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
        let mut record = CommandRecord::new(&request.id, &request.phase);
        record.identity = request.identity.clone();
        record.environment = public;
        record.forwarded_environment = request.pass_env.clone();
        record.argv = request.argv.clone();
        record.started_at = now();
        let started = Instant::now();
        let deadline = started + Duration::from_secs(request.timeout_seconds);
        // Fresh, exclusive control storage outside every observed root. A receipt
        // is separate from application streams and is never inferred from CLI text.
        let nonce = fresh_nonce()?;
        let directory = format!("/.theseus-{nonce}");
        ensure!(
            self.observed_roots
                .iter()
                .all(|root| !crate::recipe::validate::contained(&directory, root)
                    && !crate::recipe::validate::contained(root, &directory)),
            "control directory overlaps observed roots"
        );
        let prepared = (|| {
            // Interruption verifies ownership before any container mutation,
            // including creation of its private control layout.
            if interrupted {
                let state = self
                    .inspect_owned(resource, deadline, cancel)?
                    .context("owned container missing before interruption")?;
                ensure!(state.running, "owned container is not running");
            }
            self.control(
                resource,
                r#"umask 077; mkdir "$1" && chmod 755 "$1" && mkdir "$1/command" && chown "$2" "$1/command""#,
                &[
                    &directory,
                    &format!("{}:{}", request.identity.uid, request.identity.gid),
                ],
                if interrupted { deadline.min(Instant::now() + Duration::from_secs(1)) } else { deadline },
                cancel,
            )
        })();
        Ok((
            record,
            Launch {
                files: Some(files),
                stdout_path,
                stderr_path,
                started,
                deadline,
                directory,
                nonce,
                actual,
                prepared: prepared.map(|_| ()),
            },
        ))
    }

    pub(super) fn execution_command(
        &self,
        resource: &OwnedResource,
        request: &CommandRequest,
        launch: &Launch,
        private_environment: &BTreeMap<String, String>,
    ) -> Command {
        let receipt = launch.receipt();
        let mut command = self.command();
        command.args([
            "exec", "--user", &format!("{}:{}", request.identity.uid, request.identity.gid),
            "--workdir", "/workspace", &resource.container_id,
            "/bin/sh", "-c",
            // The wrapper's environment stays independent of forwarded PATH/HOME.
            // Requested argv (including shell scripts) runs in its own child.
            "receipt=$1; nonce=$2; shift 2; \"$@\"; status=$?; printf '%s:%s\\n' \"$nonce\" \"$status\" > \"$receipt\" || exit 125; exit 0",
            "theseus-command", &receipt, &launch.nonce, "/usr/bin/env", "-i",
        ]);
        // Engine control data has a separate input and never enters public
        // environment metadata or the list of forwarded host variables.
        let mut actual = launch.actual.clone();
        actual.extend(
            private_environment
                .iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        for (key, value) in actual {
            let mut pair = key;
            pair.push("=");
            pair.push(value);
            command.arg(pair);
        }
        command.args(&request.argv);
        command
    }

    pub(super) fn verify_completion(
        &mut self,
        resource: &OwnedResource,
        launch: &Launch,
        status: Option<i32>,
        cancel: &CancellationToken,
    ) -> Result<i32> {
        self.verify_completion_until(resource, launch, status, launch.deadline, cancel)
    }

    pub(super) fn verify_completion_until(
        &mut self,
        resource: &OwnedResource,
        launch: &Launch,
        status: Option<i32>,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<i32> {
        ensure!(
            status == Some(0),
            "Docker command control failed (status {:?}); application completion unknown",
            status
        );
        let text = self.control(
            resource,
            "test -f \"$1\" && test ! -L \"$1\" && cat \"$1\" && rm -f \"$1\"",
            &[&launch.receipt()],
            deadline,
            cancel,
        )?;
        let status = text
            .strip_prefix(&format!("{}:", launch.nonce))
            .and_then(|value| value.strip_suffix('\n'))
            .filter(|value| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|status| (0..=255).contains(status));
        status.context("missing or invalid application completion receipt")
    }

    pub(super) fn execute_ordinary(
        &mut self,
        resource: &OwnedResource,
        request: CommandRequest,
        store: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        let (mut record, mut launch) = self.prepare_execution(resource, &request, store, cancel)?;
        let mut command = self.execution_command(resource, &request, &launch, &BTreeMap::new());
        let deadline = launch.deadline;
        let started = launch.started;
        if let Err(error) = &launch.prepared {
            record.error = Some(format!("application completion unverified: {error:#}"));
            record.cancelled = cancel.is_cancelled();
            record.timed_out = !record.cancelled && Instant::now() >= deadline;
            record.duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            record.outcome = if record.cancelled {
                CommandOutcome::Cancelled
            } else if record.timed_out {
                CommandOutcome::TimedOut
            } else {
                CommandOutcome::Error
            };
            record.stdout = Some(LogReference {
                path: launch.stdout_path,
                bytes: 0,
                truncated: false,
            });
            record.stderr = Some(LogReference {
                path: launch.stderr_path,
                bytes: 0,
                truncated: false,
            });
            return Ok(ExecutedCommand {
                record,
                stdout: String::new(),
                stdout_utf8: false,
                stdout_complete: false,
            });
        }
        let output = process::run(
            &mut command,
            deadline.saturating_duration_since(Instant::now()),
            cancel,
            launch.files.take(),
            request.log_limit.min(crate::recipe::DEFAULT_LOG_LIMIT) as usize,
            || self.stop(resource),
        )?;
        record.error = output.error;
        record.timed_out = output.timed_out;
        record.cancelled = output.cancelled;
        record.skip_reason = None;
        let completion = (|| -> Result<i32> {
            ensure!(
                !output.supervised_stop
                    && !output.cancelled
                    && !output.timed_out
                    && record.error.is_none(),
                "application execution interrupted"
            );
            self.verify_completion(resource, &launch, output.status, cancel)
        })();
        match completion {
            Ok(status) => record.exit_status = Some(status),
            Err(error) if record.error.is_none() && !record.timed_out && !record.cancelled => {
                record.error = Some(format!("application completion unverified: {error:#}"))
            }
            Err(_) => {}
        }
        // Cancellation/deadlines apply to preparation and receipt acquisition too.
        record.cancelled |= cancel.is_cancelled();
        record.timed_out |= !record.cancelled && Instant::now() >= deadline;
        record.duration_ms =
            (started.elapsed().as_millis().min(u64::MAX as u128) as u64).max(output.duration_ms);
        record.outcome = if record.cancelled {
            CommandOutcome::Cancelled
        } else if record.timed_out {
            CommandOutcome::TimedOut
        } else if record.error.is_some() {
            CommandOutcome::Error
        } else if record.exit_status == Some(request.expected_exit) {
            CommandOutcome::Succeeded
        } else {
            CommandOutcome::Failed
        };
        record.stdout = Some(LogReference {
            path: launch.stdout_path,
            bytes: output.stdout.stored,
            truncated: output.stdout.truncated,
        });
        record.stderr = Some(LogReference {
            path: launch.stderr_path,
            bytes: output.stderr.stored,
            truncated: output.stderr.truncated,
        });
        let stdout_complete = !output.stdout.truncated
            && record.error.is_none()
            && !record.cancelled
            && !record.timed_out;
        Ok(ExecutedCommand {
            record,
            stdout_utf8: std::str::from_utf8(&output.stdout.bytes).is_ok(),
            stdout: String::from_utf8_lossy(&output.stdout.bytes).into_owned(),
            stdout_complete,
        })
    }
}
