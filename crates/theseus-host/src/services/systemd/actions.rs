//! Owned start/stop actions and job-signal progress.
use super::sdk::{Rpc, invalid};
use super::{
    MANAGER_IFACE, MANAGER_PATH, OwnedActionProgress, PendingAction, RegistrationOwner,
    ServiceAction, SystemdManager, remaining,
};
use crate::fs::{HostDir, RootPolicy};
use std::{io, sync::atomic::AtomicBool, time::Instant};

impl SystemdManager {
    pub fn action(
        &mut self,
        owner: &mut RegistrationOwner,
        action: ServiceAction,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<OwnedActionProgress> {
        if self.pending.is_some() {
            return Err(invalid("original action retained; query it without replay"));
        }
        match action {
            ServiceAction::Start if !self.authority.allowed_start => {
                return Err(invalid("Start not granted"));
            }
            ServiceAction::Stop if !self.authority.allowed_stop => {
                return Err(invalid("Stop not granted"));
            }
            _ => {}
        }
        self.verify_owner(owner, deadline, cancel)?;
        // Discard only signals observed before this original dispatch.
        if let Some(signals) = &mut self.signals {
            signals.take()?;
        }
        let method = match action {
            ServiceAction::Start => "StartUnit",
            ServiceAction::Stop => "StopUnit",
        };
        let mut message = self.bus.method(
            &self.authority.manager_unique_name,
            MANAGER_PATH,
            MANAGER_IFACE,
            method,
        )?;
        message.append(&self.authority.unit)?;
        message.append("fail")?;
        let usec = remaining(deadline, cancel)?;
        // Retain original filesystem authority before the uncertain native send.
        let definition_file = owner._file.try_clone()?;
        let definition_root = HostDir::open(
            &self.authority.definition_root,
            RootPolicy::AllComponentsNoFollow,
        )?;
        if !self.root.same_object(&definition_root)? {
            self.revoked = true;
            owner.revoked = true;
            return Err(invalid("definition root changed before action"));
        }
        self.pending = Some(PendingAction {
            rpc: None,
            _definition_file: definition_file,
            _definition_root: definition_root,
            job: None,
            events: Vec::new(),
            progress: OwnedActionProgress::Pending { job: None },
        });
        match self.bus.call(&message, usec) {
            Ok(rpc) => self.pending.as_mut().expect("original action prepared").rpc = Some(rpc),
            Err(error) => {
                let progress = OwnedActionProgress::Unknown {
                    job: None,
                    reason: error.to_string(),
                };
                self.pending
                    .as_mut()
                    .expect("original action prepared")
                    .progress = progress.clone();
                self.retained_errors.push(error);
                return Ok(progress);
            }
        }
        self.poll_action(deadline, cancel)
    }
    pub fn pending_action(&self) -> Option<OwnedActionProgress> {
        self.pending.as_ref().map(|p| p.progress.clone())
    }
    pub(super) fn advance_action(&mut self) -> io::Result<Option<OwnedActionProgress>> {
        let pending = self
            .pending
            .as_mut()
            .ok_or_else(|| invalid("no original systemd action"))?;
        if pending.job.is_none()
            && let Some(mut reply) = pending.rpc.as_ref().and_then(Rpc::reply)
        {
            if let Some(error) = reply.error()? {
                return Err(io::Error::other(error));
            }
            pending.job = Some(reply.string(b'o')?);
        }
        if let Some(signals) = &mut self.signals {
            for mut signal in signals.take()? {
                let _id = signal.u32(b'u')?;
                let path = signal.string(b'o')?;
                let unit = signal.string(b's')?;
                let result = signal.string(b's')?;
                if pending.events.len() == 64 {
                    return Err(invalid("original action event bound exceeded"));
                }
                pending.events.push((path, unit, result));
            }
        }
        if let Some(job) = &pending.job
            && let Some((_, _, result)) = pending
                .events
                .iter()
                .find(|(path, unit, _)| path == job && unit == &self.authority.unit)
        {
            let progress = OwnedActionProgress::Terminal {
                job: job.clone(),
                result: result.clone(),
            };
            pending.progress = progress.clone();
            return Ok(Some(progress));
        }
        pending.progress = OwnedActionProgress::Pending {
            job: pending.job.clone(),
        };
        Ok(None)
    }
    pub fn poll_action(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<OwnedActionProgress> {
        if self.pending.is_none() {
            return Err(invalid("no original action to query"));
        }
        if self.pending.as_ref().is_some_and(|p| p.rpc.is_none()) {
            return Ok(self.pending_action().expect("original action retained"));
        }
        if matches!(
            self.pending_action(),
            Some(OwnedActionProgress::Terminal { .. })
        ) {
            return Ok(self.pending_action().unwrap());
        }
        let result: io::Result<OwnedActionProgress> = (|| {
            self.verify_context(deadline, cancel)?;
            loop {
                remaining(deadline, cancel)?;
                if let Some(progress) = self.advance_action()? {
                    return Ok(progress);
                }
                let mut count = 0;
                while self.bus.process()? {
                    count += 1;
                    if count == 64 {
                        break;
                    }
                }
                if let Some(progress) = self.advance_action()? {
                    return Ok(progress);
                }
                self.bus.poll(25)?;
            }
        })();
        match result {
            Ok(progress) => Ok(progress),
            Err(error) => {
                let pending = self
                    .pending
                    .as_mut()
                    .expect("original pending action retained");
                let progress = OwnedActionProgress::Unknown {
                    job: pending.job.clone(),
                    reason: error.to_string(),
                };
                pending.progress = progress.clone();
                if self.retained_errors.len() < 16 {
                    self.retained_errors.push(error);
                }
                Ok(progress)
            }
        }
    }
    /// Release only a proved terminal job receipt before another durable action.
    pub fn take_terminal_action(&mut self) -> io::Result<OwnedActionProgress> {
        if !matches!(
            self.pending_action(),
            Some(OwnedActionProgress::Terminal { .. })
        ) {
            return Err(invalid("original action is not terminal"));
        }
        Ok(self
            .pending
            .take()
            .expect("checked original action")
            .progress)
    }
}
