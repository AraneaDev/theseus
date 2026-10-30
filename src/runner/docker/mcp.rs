//! Interactive MCP acquisition using the ordinary launch and completion boundary.
use super::*;
use crate::mcp::{McpIssue, McpIssueKind, McpProbe, McpSession, McpShutdown};
use process::{DuplexDriver, DuplexOutput, run_duplex};

impl DuplexDriver for McpSession {
    fn pending_write(&self) -> &[u8] {
        self.pending_write()
    }
    fn written(&mut self, count: usize) -> Result<()> {
        self.written(count)
    }
    fn receive(&mut self, bytes: &[u8]) {
        self.receive(bytes);
    }
    fn eof(&mut self) {
        self.eof();
    }
    fn cancel(&mut self) {
        self.cancel();
    }
    fn wants_close(&self) -> bool {
        self.wants_close()
    }
}

impl Docker {
    pub(super) fn execute_mcp(
        &mut self,
        resource: &OwnedResource,
        request: CommandRequest,
        probe: McpProbe,
        store: &ReportStore,
        cancel: &CancellationToken,
    ) -> Result<ExecutedMcp> {
        let started = Instant::now();
        let deadline = started + Duration::from_secs(probe.timeout_seconds.min(120));
        probe.validate()?;
        ensure!(
            request.argv == probe.argv
                && request.env == probe.env
                && request.pass_env.is_empty()
                && request.timeout_seconds == probe.timeout_seconds
                && request.expected_exit == 0
                && request.identity.uid == 10001
                && request.identity.gid == 10001
                && request.identity.home == "/home/theseus",
            "MCP request and probe launch fields disagree"
        );
        let fallback_probe = probe.clone();
        let session = McpSession::new(probe)?;
        let (mut record, mut launch) =
            self.prepare_mcp_execution(resource, &request, store, cancel, deadline)?;
        let mut command =
            self.execution_command_mode(resource, &request, &launch, &BTreeMap::new(), true);
        let mut available = true;
        let mut removed = false;
        let mut cleanup_deadline = None;
        let mut abort = || -> Result<()> {
            available = false;
            let until =
                *cleanup_deadline.get_or_insert_with(|| Instant::now() + Duration::from_secs(3));
            self.remove_owned_until(resource, until)?;
            removed = true;
            Ok(())
        };
        // The caller retains one second for receipt acquisition. The duplex
        // supervisor reserves its own final second for stdin EOF and draining.
        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .saturating_sub(Duration::from_secs(1));
        let mut evidence;
        let mut stdout = String::new();
        let mut stdout_utf8 = false;
        let mut stdout_complete = false;
        if launch.prepared.is_err() || timeout <= Duration::from_secs(1) || cancel.is_cancelled() {
            record.error = Some(match &launch.prepared {
                Err(error) => format!("MCP preparation failed: {error:#}"),
                Ok(()) => "MCP preparation exhausted exchange budget or was cancelled".into(),
            });
            record.cancelled = cancel.is_cancelled();
            record.timed_out = !record.cancelled && timeout <= Duration::from_secs(1);
            if let Err(error) = abort() {
                append(&mut record.error, format!("owned MCP cleanup: {error:#}"));
            }
            evidence = session.finish(McpShutdown::NotStarted);
            record.stdout = Some(LogReference {
                path: launch.stdout_path.clone(),
                bytes: 0,
                truncated: false,
            });
            record.stderr = Some(LogReference {
                path: launch.stderr_path.clone(),
                bytes: 0,
                truncated: false,
            });
        } else {
            let acquired: Result<DuplexOutput<McpSession>> = run_duplex(
                &mut command,
                timeout,
                cancel,
                launch.files.take(),
                request.log_limit.min(262144) as usize,
                request.log_limit.min(65536) as usize,
                session,
                &mut abort,
            );
            match acquired {
                Ok(output) => {
                    record.error = output.process.error;
                    if let Some(error) = output.io_error {
                        append(&mut record.error, error);
                    }
                    record.cancelled = output.process.cancelled;
                    record.timed_out = output.process.timed_out;
                    let clean = !output.forced_abort
                        && output.stdin_closed
                        && output.stdout_eof
                        && !output.process.stdout.truncated
                        && record.error.is_none()
                        && output.process.status == Some(0);
                    if clean {
                        // Cancellation can only finish bounded receipt/cleanup work;
                        // it never restarts the protocol or extends the outer deadline.
                        let receipt_cancel = CancellationToken::default();
                        match self.verify_completion_until(
                            resource,
                            &launch,
                            output.process.status,
                            deadline,
                            if cancel.is_cancelled() {
                                &receipt_cancel
                            } else {
                                cancel
                            },
                        ) {
                            Ok(status) => {
                                record.exit_status = Some(status);
                                stdout_complete = true;
                            }
                            Err(error) => append(
                                &mut record.error,
                                format!("MCP completion unverified: {error:#}"),
                            ),
                        }
                        record.timed_out |= !cancel.is_cancelled() && Instant::now() >= deadline;
                    } else if record.error.is_none() && !record.cancelled && !record.timed_out {
                        record.error = Some("MCP control exit, EOF or capture incomplete; application completion unknown".into());
                    }
                    let shutdown = if stdout_complete {
                        McpShutdown::Graceful
                    } else if removed {
                        McpShutdown::ForcedOwnedRemoval
                    } else {
                        McpShutdown::Unverified
                    };
                    evidence = output.driver.finish(shutdown);
                    evidence.stdout_bytes = output.stdout_bytes;
                    evidence.stdout_complete &=
                        !output.process.stdout.truncated && output.stdout_eof;
                    stdout_utf8 = std::str::from_utf8(&output.process.stdout.bytes).is_ok();
                    stdout = String::from_utf8_lossy(&output.process.stdout.bytes).into_owned();
                    record.stdout = Some(LogReference {
                        path: launch.stdout_path.clone(),
                        bytes: output.process.stdout.stored,
                        truncated: output.process.stdout.truncated,
                    });
                    record.stderr = Some(LogReference {
                        path: launch.stderr_path.clone(),
                        bytes: output.process.stderr.stored,
                        truncated: output.process.stderr.truncated,
                    });
                    if !stdout_complete && !output.forced_abort {
                        // No verified normal completion: bound the guest lifetime too.
                        let until = *cleanup_deadline
                            .get_or_insert_with(|| Instant::now() + Duration::from_secs(3));
                        available = false;
                        match self.remove_owned_until(resource, until) {
                            Ok(()) => evidence.shutdown = McpShutdown::ForcedOwnedRemoval,
                            Err(error) => {
                                append(&mut record.error, format!("owned MCP cleanup: {error:#}"))
                            }
                        }
                    }
                }
                Err(error) => {
                    // Exceptional worker/setup failure cannot return its driver.
                    record.error = Some(format!("MCP acquisition failed: {error:#}"));
                    available = false;
                    let until = *cleanup_deadline
                        .get_or_insert_with(|| Instant::now() + Duration::from_secs(3));
                    let shutdown = match self.remove_owned_until(resource, until) {
                        Ok(()) => McpShutdown::ForcedOwnedRemoval,
                        Err(error) => {
                            append(&mut record.error, format!("owned MCP cleanup: {error:#}"));
                            McpShutdown::Unverified
                        }
                    };
                    evidence = McpSession::new(fallback_probe)
                        .expect("previously validated probe")
                        .finish(shutdown);
                }
            }
        }
        record.cancelled |= cancel.is_cancelled();
        if !stdout_complete {
            record.exit_status = None;
        }
        if evidence.issue.is_none()
            && let Some(message) = &record.error
        {
            let mut diagnostic = message.clone();
            while diagnostic.len() > 512 {
                diagnostic.pop();
            }
            evidence.issue = Some(McpIssue {
                kind: McpIssueKind::Transport,
                message: diagnostic,
            });
            evidence.protocol_complete = false;
        }
        record.duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        record.outcome = if record.cancelled {
            CommandOutcome::Cancelled
        } else if record.timed_out {
            CommandOutcome::TimedOut
        } else if record.error.is_some() {
            CommandOutcome::Error
        } else if record.exit_status == Some(0) {
            CommandOutcome::Succeeded
        } else {
            CommandOutcome::Failed
        };
        Ok(ExecutedMcp {
            command: ExecutedCommand {
                record,
                stdout,
                stdout_utf8,
                stdout_complete,
            },
            evidence,
            container_available: available,
        })
    }
}
fn append(target: &mut Option<String>, message: String) {
    if let Some(previous) = target {
        previous.push_str("; ");
        previous.push_str(&message);
    } else {
        *target = Some(message);
    }
}
