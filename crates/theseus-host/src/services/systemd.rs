//! Explicit-address systemd observation and original-registration runtime controls.
//!
//! The supplied provider owns the exclusive registration/load-path context. A
//! manager bus owner is deliberately never treated as a unit generation.
mod sdk;
use crate::fs::{FileFingerprint, HostDir};
use sdk::{Bus, JobSignals, Rpc};
use std::{fs::File, io, path::PathBuf};
#[cfg(test)]
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

pub use super::endpoint::UnixPath;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemdScope {
    User,
    System,
}
/// Provider-authenticated reservation. Receipt bytes are verified by the helper;
/// this mechanism additionally checks the live manager and retained roots.
#[derive(Clone, Debug)]
pub struct SystemdAuthority {
    pub endpoint: UnixPath,
    pub scope: SystemdScope,
    pub bus_uid: u32,
    pub manager_uid: u32,
    pub manager_unique_name: String,
    pub manager_version: String,
    pub unit: String,
    pub definition_root: PathBuf,
    pub unit_load_paths: Vec<String>,
    pub ownership_id: String,
    pub context_receipt: String,
    pub allowed_start: bool,
    pub allowed_stop: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemdDefinition {
    pub executable: String,
    /// Complete argv including argv[0], as exposed by ExecStart.
    pub arguments: Vec<String>,
    pub environment: Vec<String>,
    pub working_directory: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceObservation {
    pub definition_present: bool,
    /// Bounded bytes read through the retained root with before/after identity
    /// checks. Diagnostic commitment evidence, never reconstruction authority.
    pub definition_bytes: Option<Vec<u8>>,
    pub unit_file_state: Option<String>,
    /// None means not loaded; it does not mean the definition is absent.
    pub loaded: Option<LoadedService>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedService {
    pub unit: String,
    pub names: Vec<String>,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub fragment_path: String,
    pub drop_in_paths: Vec<String>,
    pub invocation_id: Vec<u8>,
    pub need_daemon_reload: bool,
    pub service_type: String,
    pub restart: String,
    pub definition: Option<SystemdDefinition>,
    pub main_pid: u32,
    pub result: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnedActionProgress {
    Pending {
        job: Option<String>,
    },
    /// Job completion is not application readiness or process-tree settlement.
    Terminal {
        job: String,
        result: String,
    },
    Unknown {
        job: Option<String>,
        reason: String,
    },
}
/// Original qualified registration. Never serializable or reconstructible by PID.
pub struct RegistrationOwner {
    _file: File,
    prior_files: Vec<File>,
    transition_receipts: Vec<String>,
    fingerprint: FileFingerprint,
    definition_bytes: Vec<u8>,
    definition: SystemdDefinition,
    manager_instance: [u8; 32],
    install_receipt: String,
    revoked: bool,
}
impl RegistrationOwner {
    fn record_transition(
        &mut self,
        file: File,
        fingerprint: FileFingerprint,
        bytes: Vec<u8>,
        definition: &SystemdDefinition,
        receipt: String,
    ) -> io::Result<()> {
        if self.revoked
            || self.transition_receipts.len() >= 128
            || receipt == self.install_receipt
            || self.transition_receipts.contains(&receipt)
        {
            return Err(io::Error::other("original owner transition unavailable"));
        }
        self.prior_files
            .push(std::mem::replace(&mut self._file, file));
        self.fingerprint = fingerprint;
        self.definition_bytes = bytes;
        self.definition = definition.clone();
        self.transition_receipts.push(receipt);
        Ok(())
    }
    pub fn install_receipt(&self) -> &str {
        &self.install_receipt
    }
    pub fn is_revoked(&self) -> bool {
        self.revoked
    }
    pub fn definition_bytes(&self) -> &[u8] {
        &self.definition_bytes
    }
}
struct PendingAction {
    rpc: Option<Rpc>,
    _definition_file: File,
    _definition_root: HostDir,
    job: Option<String>,
    events: Vec<(String, String, String)>,
    progress: OwnedActionProgress,
}
/// Non-Send connection, callbacks and native owners stay on the existing worker.
pub struct SystemdManager {
    bus: Bus,
    authority: SystemdAuthority,
    root: HostDir,
    instance: [u8; 32],
    revoked: bool,
    signals: Option<JobSignals>,
    pending: Option<PendingAction>,
    retained_errors: Vec<io::Error>,
}
const MANAGER: &str = "org.freedesktop.systemd1";
const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
const MANAGER_IFACE: &str = "org.freedesktop.systemd1.Manager";
const UNIT_IFACE: &str = "org.freedesktop.systemd1.Unit";
const SERVICE_IFACE: &str = "org.freedesktop.systemd1.Service";

mod actions;
mod connection;
mod observation;
mod registration;
mod validation;

use connection::remaining;
use validation::validate_definition_source;

// If unresolved side effects remain, retain connection, callbacks, library and
// original errors through process exit. Dropping an unknown is not cleanup proof.
impl Drop for SystemdManager {
    fn drop(&mut self) {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| !matches!(p.progress, OwnedActionProgress::Terminal { .. }))
        {
            if let Some(pending) = self.pending.take() {
                std::mem::forget(pending);
            }
            if let Some(signals) = self.signals.take() {
                std::mem::forget(signals);
            }
            // SDK refs in leaked callbacks alone do not retain the bus; preserve
            // its original native reference as well as the library allocation.
            self.bus.retain_on_drop();
            std::mem::forget(std::mem::take(&mut self.retained_errors));
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod model_tests;

#[cfg(test)]
mod definition_model_tests;

#[cfg(test)]
mod sut_transition_owner_tests;
