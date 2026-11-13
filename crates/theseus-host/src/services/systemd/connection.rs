//! Manager connection, bounded RPC, property reads and context verification.
use super::sdk::{Bus, Message, Rpc, invalid};
use super::{MANAGER, MANAGER_IFACE, MANAGER_PATH, SystemdAuthority, SystemdManager};
use crate::fs::{HostDir, RootPolicy};
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(super) fn remaining(deadline: Instant, cancel: &AtomicBool) -> io::Result<u64> {
    if cancel.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "systemd operation cancelled",
        ));
    }
    let left = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "systemd deadline elapsed"))?;
    if left > Duration::from_secs(30) {
        return Err(invalid("systemd deadline exceeds 30 seconds"));
    }
    Ok(left.as_micros().clamp(1, u64::MAX as u128) as u64)
}
impl SystemdManager {
    pub fn connect(
        authority: SystemdAuthority,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        authority.validate()?;
        remaining(deadline, cancel)?;
        let root = HostDir::open(
            &authority.definition_root,
            RootPolicy::AllComponentsNoFollow,
        )?;
        let mut instance = [0; 32];
        crate::random_bytes(&mut instance)?;
        let bus = Bus::new(&authority.endpoint.address())?;
        let mut manager = Self {
            bus,
            authority,
            root,
            instance,
            revoked: false,
            signals: None,
            pending: None,
            retained_errors: Vec::new(),
        };
        manager.verify_context(deadline, cancel)?;
        manager.signals = Some(manager.bus.job_signals(
            &manager.authority.manager_unique_name,
            remaining(deadline, cancel)?,
        )?);
        manager.manager_call("Subscribe", &[], deadline, cancel)?;
        Ok(manager)
    }
    /// Provider loss of the original exclusive context is permanent.
    pub fn revoke_context(&mut self) {
        self.revoked = true;
    }
    pub fn is_revoked(&self) -> bool {
        self.revoked
    }
    pub(super) fn rpc_wait(
        &self,
        rpc: &Rpc,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Message> {
        loop {
            remaining(deadline, cancel)?;
            if let Some(message) = rpc.reply() {
                if let Some(error) = message.error()? {
                    return Err(io::Error::other(error));
                }
                return Ok(message);
            }
            let mut count = 0;
            while self.bus.process()? {
                count += 1;
                if count == 64 {
                    break;
                }
            }
            if rpc.reply().is_none() {
                self.bus.poll(25)?;
            }
        }
    }
    pub(super) fn call(
        &self,
        target: (&str, &str, &str, &str),
        args: &[&str],
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Message> {
        let (dest, path, interface, method) = target;
        let mut message = self.bus.method(dest, path, interface, method)?;
        for arg in args {
            message.append(arg)?;
        }
        let rpc = self.bus.call(&message, remaining(deadline, cancel)?)?;
        self.rpc_wait(&rpc, deadline, cancel)
    }
    fn manager_call(
        &self,
        method: &str,
        args: &[&str],
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Message> {
        self.call(
            (
                &self.authority.manager_unique_name,
                MANAGER_PATH,
                MANAGER_IFACE,
                method,
            ),
            args,
            deadline,
            cancel,
        )
    }
    pub(super) fn property(
        &self,
        path: &str,
        interface: &str,
        name: &str,
        signature: &std::ffi::CStr,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Message> {
        let mut reply = self.call(
            (
                &self.authority.manager_unique_name,
                path,
                "org.freedesktop.DBus.Properties",
                "Get",
            ),
            &[interface, name],
            deadline,
            cancel,
        )?;
        reply.require_enter(b'v', signature)?;
        Ok(reply)
    }
    pub(super) fn string_property(
        &self,
        path: &str,
        interface: &str,
        name: &str,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<String> {
        self.property(path, interface, name, c"s", deadline, cancel)?
            .string(b's')
    }
    pub(super) fn strings_property(
        &self,
        path: &str,
        interface: &str,
        name: &str,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Vec<String>> {
        self.property(path, interface, name, c"as", deadline, cancel)?
            .strings()
    }
    pub(super) fn verify_context(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        // Read-only original-operation queries remain possible after revocation.
        // No successful read ever clears the sticky mutation latch.
        let result = (|| {
            remaining(deadline, cancel)?;
            if self.bus.peer_uid()? != self.authority.bus_uid {
                return Err(invalid("wrong connected bus peer principal"));
            }
            let current = HostDir::open(
                &self.authority.definition_root,
                RootPolicy::AllComponentsNoFollow,
            )?;
            if !self.root.same_object(&current)? {
                return Err(invalid("definition root replaced"));
            }
            let mut reply = self.call(
                (
                    "org.freedesktop.DBus",
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "GetNameOwner",
                ),
                &[MANAGER],
                deadline,
                cancel,
            )?;
            if reply.string(b's')? != self.authority.manager_unique_name {
                return Err(invalid("manager owner changed"));
            }
            let mut reply = self.call(
                (
                    "org.freedesktop.DBus",
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "GetConnectionUnixUser",
                ),
                &[&self.authority.manager_unique_name],
                deadline,
                cancel,
            )?;
            if reply.u32(b'u')? != self.authority.manager_uid {
                return Err(invalid("manager principal changed"));
            }
            if self.string_property(MANAGER_PATH, MANAGER_IFACE, "Version", deadline, cancel)?
                != self.authority.manager_version
            {
                return Err(invalid("unqualified manager version"));
            }
            let paths =
                self.strings_property(MANAGER_PATH, MANAGER_IFACE, "UnitPath", deadline, cancel)?;
            if paths != self.authority.unit_load_paths {
                return Err(invalid("manager load paths outside exclusive namespace"));
            }
            Ok(())
        })();
        if result.is_err() {
            self.revoked = true;
        }
        result
    }
}
