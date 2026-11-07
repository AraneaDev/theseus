use super::{CommandCompletion, model::*};
use crate::process::{
    AcquiredChild, HostCleanup, InputMode, NativeCommand, OwnedChild, SpawnFailure,
};
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
/// One original native command. Timeout/cancellation retain its child, pipes,
/// bounded buffers and original spawn/read/wait errors; polling never respawns.
pub(super) struct CommandOwner {
    child: Option<AcquiredChild>,
    failure: Option<SpawnFailure>,
    evidence: CommandEvidence,
    eof: [bool; 2],
    complete: bool,
    errors: Vec<io::Error>,
}
impl CommandOwner {
    pub(super) fn spawn(command: &NativeCommand, qualification: PrintQualification) -> Self {
        let result = OwnedChild::spawn(command, InputMode::Null);
        let (child, failure, complete) = match result {
            Ok(child) => (Some(child), None, false),
            Err(failure) => {
                let complete = matches!(failure.cleanup, HostCleanup::Complete);
                (None, Some(failure), complete)
            }
        };
        Self {
            child,
            failure,
            evidence: CommandEvidence {
                program: command.program.clone().into(),
                arguments: command.args.clone(),
                stdout: vec![],
                stderr: vec![],
                exit_code: None,
                truncated: false,
                qualification,
            },
            eof: [false; 2],
            complete,
            errors: vec![],
        }
    }
    pub(super) fn advance(&mut self) -> io::Result<bool> {
        if self.complete {
            return Ok(true);
        }
        let Some(child) = self.child.as_mut() else {
            return Ok(false);
        };
        for (index, pipe, bytes) in [
            (0, &mut child.stdout, &mut self.evidence.stdout),
            (1, &mut child.stderr, &mut self.evidence.stderr),
        ] {
            if self.eof[index] {
                continue;
            }
            // Bounded work per poll even if a command continuously writes.
            for _ in 0..8 {
                let mut buf = [0; 4096];
                match pipe.try_read(&mut buf) {
                    Ok(0) => {
                        self.eof[index] = true;
                        break;
                    }
                    Ok(count) => {
                        let keep = count.min(OUTPUT_LIMIT.saturating_sub(bytes.len()));
                        bytes.extend_from_slice(&buf[..keep]);
                        if keep < count {
                            self.evidence.truncated = true;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
        }
        if self.eof == [true, true] && child.child.exited_reserved()? {
            // This only releases the owned launchctl process/group, never any
            // launchd-managed job or a reported service PID.
            if let Some(status) = child.child.try_reap_and_release()? {
                self.evidence.exit_code = status.code();
                self.complete = true;
            }
        }
        Ok(self.complete)
    }
    pub(super) fn poll(&mut self, deadline: Instant, cancel: &AtomicBool) -> io::Result<bool> {
        loop {
            remaining(deadline, cancel)?;
            if self.advance()? {
                return Ok(true);
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    pub(super) fn evidence(&self) -> CommandEvidence {
        self.evidence.clone()
    }
    pub(super) fn error(&mut self, error: io::Error) -> String {
        let reason = error.to_string();
        if self.errors.len() < 16 {
            self.errors.push(error);
        }
        reason
    }
    pub(super) fn completion(&self) -> CommandCompletion {
        if self.failure.is_some() {
            CommandCompletion::SpawnFailed
        } else {
            match self.evidence.exit_code {
                Some(code) => CommandCompletion::Exited(code),
                None => CommandCompletion::NoNormalExit,
            }
        }
    }
    pub(super) fn outcome(&self) -> String {
        if let Some(failure) = &self.failure {
            return failure.to_string();
        }
        match self.evidence.exit_code {
            Some(code) => format!(
                "launchctl exit {code}; runtime settlement and readiness require independent observations"
            ),
            None => "launchctl terminated without a normal exit status".into(),
        }
    }
}
impl Drop for CommandOwner {
    fn drop(&mut self) {
        if !self.complete {
            // Dropping uncertainty is not permission to kill/replay. Keep original
            // process/pipe/native cleanup owners until helper/provider disposition.
            if let Some(child) = self.child.take() {
                std::mem::forget(child);
            }
            if let Some(failure) = self.failure.take() {
                std::mem::forget(failure);
            }
            std::mem::forget(std::mem::take(&mut self.errors));
        }
    }
}
pub(super) fn remaining(deadline: Instant, cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "launchd operation cancelled; original owners retained",
        ));
    }
    let left = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "launchd operation deadline elapsed; original owners retained",
            )
        })?;
    if left > std::time::Duration::from_secs(30) {
        return Err(invalid("launchd deadline exceeds 30 seconds"));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    #[ignore = "owned command child entry; invoked by the parent test"]
    fn command_child() {
        if std::env::var_os("THESEUS_LAUNCHD_COMMAND_CHILD").is_none() {
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
        use std::io::Write;
        std::io::stdout()
            .write_all(&vec![b'x'; OUTPUT_LIMIT + 1000])
            .unwrap();
        std::io::stderr().write_all(b"diagnostic").unwrap();
        std::process::exit(0);
    }
    #[test]
    fn launchd_command_cancellation_retains_original_child_and_bounded_pipes_until_poll() {
        let mut native = NativeCommand::new(std::env::current_exe().unwrap());
        native.args([
            "--ignored",
            "--exact",
            "services::launchd::command::tests::command_child",
            "--nocapture",
        ]);
        native.environment = crate::process::ChildEnvironment::Replace(vec![(
            "THESEUS_LAUNCHD_COMMAND_CHILD".into(),
            "1".into(),
        )]);
        let mut command = CommandOwner::spawn(
            &native,
            super::super::model::tests::authority().qualification,
        );
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            command
                .poll(Instant::now() + Duration::from_secs(2), &cancelled)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(command.child.is_some());
        assert!(!command.complete);
        cancelled.store(false, Ordering::Release);
        assert!(
            command
                .poll(Instant::now() + Duration::from_secs(2), &cancelled)
                .unwrap()
        );
        let evidence = command.evidence();
        assert_eq!(evidence.exit_code, Some(0));
        assert_eq!(command.completion(), CommandCompletion::Exited(0));
        assert_eq!(evidence.stdout.len(), OUTPUT_LIMIT);
        assert!(evidence.truncated);
        assert_eq!(evidence.stderr, b"diagnostic");
        assert!(
            command
                .poll(Instant::now() + Duration::from_secs(2), &cancelled)
                .unwrap()
        );
    }
    #[test]
    fn launchd_terminal_success_requires_normal_zero_exit_without_spawn_failure() {
        assert!(CommandCompletion::Exited(0).succeeded());
        assert!(!CommandCompletion::Exited(7).succeeded());
        assert!(!CommandCompletion::NoNormalExit.succeeded());
        assert!(!CommandCompletion::SpawnFailed.succeeded());
    }
    #[test]
    fn launchd_command_spawn_failure_retains_original_error_without_replay() {
        let native = NativeCommand::new("/nonexistent-theseus-launchd-model-command");
        let mut command = CommandOwner::spawn(
            &native,
            super::super::model::tests::authority().qualification,
        );
        assert!(command.advance().unwrap());
        assert!(command.failure.is_some());
        assert!(command.child.is_none());
        assert!(command.outcome().contains("NeverCreated"));
        assert_eq!(command.evidence().exit_code, None);
        assert_eq!(command.completion(), CommandCompletion::SpawnFailed);
    }
}
