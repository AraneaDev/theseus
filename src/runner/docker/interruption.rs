//! Nonce-bound cooperative pause followed by an owned, verified whole-container kill.
use super::{
    execution::Launch,
    process::{SupervisionDecision, SupervisionEvent},
    *,
};

const BARRIER_VARIABLE: &str = "THESEUS_INTERRUPT_BARRIER";

impl Docker {
    fn interruption_control(
        &mut self,
        resource: &OwnedResource,
        script: &str,
        args: &[&str],
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<String> {
        self.control(
            resource,
            script,
            args,
            deadline.min(Instant::now() + Duration::from_secs(1)),
            cancel,
        )
    }

    fn install_barrier(
        &mut self,
        resource: &OwnedResource,
        request: &CommandRequest,
        launch: &Launch,
        cancel: &CancellationToken,
    ) -> Result<String> {
        let helper = format!("{}/barrier.sh", launch.directory);
        let body = format!(
            r#"#!/bin/sh
set -eu
PATH=/usr/bin:/bin
export PATH
umask 077
set -C
printf '%s\n' '{}' > '{}/ready/pending'
mv -T -- '{}/ready/pending' '{}/ready/receipt'
while :; do sleep 3600; done
"#,
            launch.nonce, launch.directory, launch.directory, launch.directory
        );
        self.interruption_control(
            resource,
            r#"set -eu
command -v dd >/dev/null
command -v mv >/dev/null
test -d "$1" && test ! -L "$1"
mkdir "$1/ready"
chown "$2" "$1/ready"
chmod 700 "$1/ready"
umask 022
set -C
printf '%s' "$3" > "$1/barrier.sh"
chmod 644 "$1/barrier.sh"
"#,
            &[
                &launch.directory,
                &format!("{}:{}", request.identity.uid, request.identity.gid),
                &body,
            ],
            launch.deadline,
            cancel,
        )?;
        Ok(helper)
    }

    fn barrier_ready(
        &mut self,
        resource: &OwnedResource,
        launch: &Launch,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        // Parent ownership prevents the runtime identity from replacing layout
        // components. dd uses O_NOFOLLOW|O_NONBLOCK on the untrusted leaf; reading
        // at most nonce+newline+one byte rejects oversized or special receipts.
        let text = self.interruption_control(
            resource,
            r#"set -eu
test -d "$1" && test ! -L "$1"
test -d "$1/ready" && test ! -L "$1/ready"
if test -L "$1/ready/receipt"; then exit 4; fi
if ! test -e "$1/ready/receipt"; then printf waiting; exit 0; fi
test -f "$1/ready/receipt"
if test -e "$2" || test -L "$2"; then printf completed; exit 0; fi
printf ready:
dd if="$1/ready/receipt" iflag=nofollow,nonblock bs=1 count=34 2>/dev/null
"#,
            &[&launch.directory, &launch.receipt()],
            launch.deadline,
            cancel,
        )?;
        if text == "waiting" || text == "completed" {
            return Ok(false);
        }
        ensure!(
            text == format!("ready:{}\n", launch.nonce),
            "invalid interruption readiness receipt"
        );
        Ok(true)
    }

    pub(super) fn interrupt(
        &mut self,
        resource: &OwnedResource,
        request: CommandRequest,
        store: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<ExecutedCommand> {
        ensure!(
            !request.env.contains_key(BARRIER_VARIABLE)
                && !request.pass_env.iter().any(|name| name == BARRIER_VARIABLE),
            "interruption barrier variable is reserved"
        );
        let (mut record, mut launch) =
            self.prepare_interrupted_execution(resource, &request, store, cancel)?;
        let mut proof = InterruptionEvidence {
            mechanism: InterruptionMechanism::ContainerSigkill,
            readiness_verified: false,
            stopped_verified: false,
        };
        let prepared = (|| -> Result<String> {
            launch
                .prepared
                .as_ref()
                .map_err(|error| anyhow::anyhow!("{error:#}"))?;
            self.install_barrier(resource, &request, &launch, cancel)
        })();
        let mut output = None;
        match prepared {
            Err(error) => {
                record.error = Some(format!("interruption preparation unverified: {error:#}"))
            }
            Ok(helper) => {
                let mut command = self.execution_command(
                    resource,
                    &request,
                    &launch,
                    &BTreeMap::from([(BARRIER_VARIABLE.into(), helper)]),
                );
                let result = process::run_supervised(
                    &mut command,
                    launch.deadline.saturating_duration_since(Instant::now()),
                    cancel,
                    launch.files.take(),
                    request.log_limit.min(crate::recipe::DEFAULT_LOG_LIMIT) as usize,
                    |event| match event {
                        SupervisionEvent::Abort => {
                            self.stop(resource).map(|()| SupervisionDecision::Continue)
                        }
                        SupervisionEvent::Poll => {
                            if !self.barrier_ready(resource, &launch, cancel)? {
                                return Ok(SupervisionDecision::Continue);
                            }
                            let state = self
                                .inspect_owned(resource, launch.deadline, cancel)?
                                .context("owned container missing at barrier")?;
                            ensure!(
                                state.running,
                                "owned container stopped before planned interruption"
                            );
                            proof.readiness_verified = true;
                            self.owned_transition(
                                &[
                                    "container",
                                    "kill",
                                    "--signal",
                                    "KILL",
                                    &resource.container_id,
                                ],
                                launch.deadline,
                                cancel,
                            )?;
                            let state = self
                                .inspect_owned(resource, launch.deadline, cancel)?
                                .context("owned container missing after planned kill")?;
                            ensure!(!state.running, "container still running after planned kill");
                            proof.stopped_verified = true;
                            Ok(SupervisionDecision::Stop)
                        }
                    },
                );
                match result {
                    Err(error) => {
                        record.error = Some(format!("interruption execution unverified: {error:#}"))
                    }
                    Ok(result) => {
                        record.error = result.error.clone();
                        record.timed_out = result.timed_out;
                        record.cancelled = result.cancelled;
                        if !result.supervised_stop
                            && !record.timed_out
                            && !record.cancelled
                            && record.error.is_none()
                        {
                            match self.verify_completion_until(
                                resource,
                                &launch,
                                result.status,
                                launch.deadline.min(Instant::now() + Duration::from_secs(1)),
                                cancel,
                            ) {
                                Ok(status) => record.exit_status = Some(status),
                                Err(error) => {
                                    record.error = Some(format!(
                                        "application completion unverified: {error:#}"
                                    ))
                                }
                            }
                        }
                        output = Some(result);
                    }
                }
            }
        }
        record.cancelled |= cancel.is_cancelled();
        record.timed_out |= !record.cancelled && Instant::now() >= launch.deadline;
        let deliberate = output.as_ref().is_some_and(|out| out.supervised_stop)
            && proof.readiness_verified
            && proof.stopped_verified;
        if (record.cancelled
            || record.timed_out
            || record.error.is_some()
            || (!deliberate && record.exit_status.is_none()))
            && let Err(error) = self.stop(resource)
        {
            let reason = format!("owned interruption cleanup failed: {error:#}");
            record.error = Some(
                record
                    .error
                    .map_or(reason.clone(), |prior| format!("{prior}; {reason}")),
            );
        }
        record.outcome = if record.cancelled {
            CommandOutcome::Cancelled
        } else if record.timed_out {
            CommandOutcome::TimedOut
        } else if record.error.is_some() {
            CommandOutcome::Error
        } else if deliberate {
            CommandOutcome::Interrupted
        } else {
            CommandOutcome::Failed
        };
        record.duration_ms = launch.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        record.stdout = Some(LogReference {
            path: launch.stdout_path,
            bytes: output.as_ref().map_or(0, |out| out.stdout.stored),
            truncated: output.as_ref().is_some_and(|out| out.stdout.truncated),
        });
        record.stderr = Some(LogReference {
            path: launch.stderr_path,
            bytes: output.as_ref().map_or(0, |out| out.stderr.stored),
            truncated: output.as_ref().is_some_and(|out| out.stderr.truncated),
        });
        record.interruption = Some(proof);
        // No application completion is claimed for a deliberately stopped stream.
        let stdout_complete = !deliberate
            && record.exit_status.is_some()
            && record.error.is_none()
            && !record.timed_out
            && !record.cancelled
            && output.as_ref().is_some_and(|out| !out.stdout.truncated);
        Ok(ExecutedCommand {
            record,
            stdout_utf8: output
                .as_ref()
                .is_some_and(|out| std::str::from_utf8(&out.stdout.bytes).is_ok()),
            stdout: output.map_or_else(String::new, |out| {
                String::from_utf8_lossy(&out.stdout.bytes).into_owned()
            }),
            stdout_complete,
        })
    }

    pub(super) fn restart_owned(
        &mut self,
        resource: &OwnedResource,
        cancel: &CancellationToken,
    ) -> Result<RestartEvidence> {
        let deadline = Instant::now() + Duration::from_secs(3);
        let result = (|| {
            let state = self
                .inspect_owned(resource, deadline, cancel)?
                .context("owned container missing before restart")?;
            ensure!(!state.running, "restart requires a stopped owned container");
            self.owned_transition(
                &["container", "start", &resource.container_id],
                deadline,
                cancel,
            )?;
            let state = self
                .inspect_owned(resource, deadline, cancel)?
                .context("owned container missing after restart")?;
            ensure!(state.running, "container not running after restart");
            ensure!(!cancel.is_cancelled(), "restart cancelled");
            Ok(RestartEvidence {
                container_id: state.container_id,
                running_verified: true,
            })
        })();
        match result {
            Ok(proof) => Ok(proof),
            Err(error) => match self.stop(resource) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(anyhow::anyhow!(
                    "{error:#}; owned restart cleanup failed: {cleanup:#}"
                )),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn interruption_initial_control_has_one_second_cap_with_longer_command_budget() {
        let temporary = tempfile::tempdir().unwrap();
        let executable = temporary.path().join("docker");
        std::fs::write(
            &executable,
            r#"#!/usr/bin/python3
import sys,time,json
args=sys.argv[1:]
if args[0]=='container':
    print(json.dumps({'Id':args[-1], 'Config':{'Labels':{'io.theseus.owner':'v1','io.theseus.run':'run','io.theseus.scenario':'boundary'}},'State':{'Running':True}}))
    sys.exit(0)
if args[0]=='rm': sys.exit(0)
if args[args.index('-c')+1].startswith('umask'):
    time.sleep(3)
sys.exit(0)
"#,
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = crate::report::create(&temporary.path().join("run")).unwrap();
        let resource = OwnedResource {
            container_id: "a".repeat(64),
            labels: BTreeMap::from([
                (OWNER_LABEL.into(), "v1".into()),
                (RUN_LABEL.into(), "run".into()),
                (SCENARIO_LABEL.into(), "boundary".into()),
            ]),
            removed: false,
        };
        let started = Instant::now();
        let executed = Docker::new(executable)
            .execute_interrupted(
                &resource,
                CommandRequest {
                    id: "initial-control".into(),
                    phase: "boundary".into(),
                    identity: ExecutionIdentity::runtime(),
                    argv: vec!["/bin/true".into()],
                    env: BTreeMap::new(),
                    pass_env: vec![],
                    timeout_seconds: 10,
                    expected_exit: 0,
                    log_limit: 1024,
                },
                &store,
                &CancellationToken::default(),
            )
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(executed.record.outcome, CommandOutcome::Error);
        assert!(!executed.record.timed_out);
        assert!(!executed.record.interruption.unwrap().stopped_verified);
    }
}
