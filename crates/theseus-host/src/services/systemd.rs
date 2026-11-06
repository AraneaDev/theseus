//! Explicit-address systemd observation and original-registration runtime controls.
//!
//! The supplied provider owns the exclusive registration/load-path context. A
//! manager bus owner is deliberately never treated as a unit generation.
mod sdk;
use crate::fs::{FileFingerprint, HostDir, RootPolicy, fingerprint};
use sdk::{Bus, JobSignals, Message, Rpc, invalid};
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub use super::endpoint::UnixPath;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_endpoint_rejects_dbus_grammar_and_unsafe_components() {
        for path in [
            "",
            "bus",
            "/tmp/../bus",
            "/tmp/./bus",
            "/tmp//bus",
            "/tmp/bus;unixexec:path=sh",
            "unix:path=/tmp/bus",
            "/tmp/bus,guid=00",
            "/tmp/$BUS",
            "/tmp/é",
            "/tmp/a\0b",
        ] {
            assert!(UnixPath::new(path.into()).is_err(), "accepted {path:?}");
        }
        assert!(UnixPath::new(format!("/{}", "a".repeat(100))).is_err());
        assert_eq!(
            UnixPath::new("/tmp/private/bus.sock".into())
                .unwrap()
                .address(),
            "unix:path=/tmp/private/bus.sock"
        );
    }
}

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
    /// Diagnostic binding evidence, never authority to reconstruct an owner.
    pub fn definition_fingerprint(&self) -> Vec<u8> {
        self.fingerprint.canonical_bytes()
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
impl SystemdAuthority {
    pub fn validate(&self) -> io::Result<()> {
        let exact = self.unit.strip_suffix(".service").filter(|s| !s.is_empty());
        if !exact.is_some_and(|s| {
            s.len() <= 200
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        }) || !valid_unique_name(&self.manager_unique_name)
            || self.manager_version.is_empty()
            || self.manager_version.len() > 256
            || self.ownership_id.is_empty()
            || self.ownership_id.len() > 256
            || self.context_receipt.is_empty()
            || self.context_receipt.len() > 4096
            || self.unit_load_paths.is_empty()
            || self.unit_load_paths.len() > 16
            || !plain_absolute(&self.definition_root.to_string_lossy())
        {
            return Err(invalid("incomplete or ambiguous systemd authority"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for path in &self.unit_load_paths {
            if !plain_absolute(path) || !seen.insert(path) {
                return Err(invalid("invalid or duplicate unit load root"));
            }
        }
        if !self
            .unit_load_paths
            .iter()
            .any(|p| Path::new(p) == self.definition_root)
        {
            return Err(invalid("definition root outside qualified load path"));
        }
        Ok(())
    }
}
fn plain_absolute(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 4096
        && !path.as_bytes().contains(&0)
        && (path == "/"
            || !path[1..]
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == ".."))
}
fn valid_unique_name(name: &str) -> bool {
    name.starts_with(':')
        && name.len() <= 255
        && name[1..].contains('.')
        && name[1..].split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        })
}
impl SystemdDefinition {
    pub fn validate(&self) -> io::Result<()> {
        if !plain_absolute(&self.executable)
            || !plain_absolute(&self.working_directory)
            || self.arguments.is_empty()
            || self.arguments.len() > 64
            || self.environment.len() > 64
            || self
                .arguments
                .iter()
                .chain(self.environment.iter())
                .any(|s| s.as_bytes().contains(&0))
            || self.executable.len()
                + self.working_directory.len()
                + self
                    .arguments
                    .iter()
                    .chain(self.environment.iter())
                    .map(String::len)
                    .sum::<usize>()
                > 65536
        {
            return Err(invalid("unsupported or unbounded service definition"));
        }
        let mut names = std::collections::BTreeSet::new();
        for entry in &self.environment {
            let Some((name, _)) = entry.split_once('=') else {
                return Err(invalid("invalid environment assignment"));
            };
            if name.is_empty()
                || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                || !names.insert(name)
            {
                return Err(invalid("ambiguous environment assignment"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;
    fn authority() -> SystemdAuthority {
        SystemdAuthority {
            endpoint: UnixPath::new("/tmp/theseus/bus".into()).unwrap(),
            scope: SystemdScope::User,
            bus_uid: 1000,
            manager_uid: 1000,
            manager_unique_name: ":1.17".into(),
            manager_version: "262".into(),
            unit: "theseus-fixture.service".into(),
            definition_root: PathBuf::from("/tmp/theseus/units"),
            unit_load_paths: vec!["/tmp/theseus/units".into()],
            ownership_id: "reserved-context".into(),
            context_receipt: "provider-receipt".into(),
            allowed_start: true,
            allowed_stop: true,
        }
    }
    #[test]
    fn model_rejects_incomplete_or_ambiguous_authority_before_library_loading() {
        let a = authority();
        assert!(a.validate().is_ok());
        let mut a = authority();
        a.unit = "other*.service".into();
        assert!(a.validate().is_err());
        let mut a = authority();
        a.manager_unique_name = MANAGER.into();
        assert!(a.validate().is_err());
        let mut a = authority();
        a.context_receipt.clear();
        assert!(a.validate().is_err());
        let mut a = authority();
        a.unit_load_paths.push(a.unit_load_paths[0].clone());
        assert!(a.validate().is_err());
        let mut a = authority();
        a.unit_load_paths.clear();
        assert!(a.validate().is_err());
    }
    #[test]
    fn model_rejects_unbounded_or_relative_exec_definition() {
        let mut d = SystemdDefinition {
            executable: "/fixture/service".into(),
            arguments: vec!["/fixture/service".into()],
            environment: vec!["A=value".into()],
            working_directory: "/fixture".into(),
        };
        assert!(d.validate().is_ok());
        d.executable = "service".into();
        assert!(d.validate().is_err());
        d.executable = "/fixture/service".into();
        d.environment = vec!["A=1".into(), "A=2".into()];
        assert!(d.validate().is_err());
        d.environment.clear();
        d.arguments.clear();
        assert!(d.validate().is_err());
    }
}

fn remaining(deadline: Instant, cancel: &AtomicBool) -> io::Result<u64> {
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
    fn rpc_wait(&self, rpc: &Rpc, deadline: Instant, cancel: &AtomicBool) -> io::Result<Message> {
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
    fn call(
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
    fn property(
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
    fn string_property(
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
    fn strings_property(
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
    fn verify_context(&mut self, deadline: Instant, cancel: &AtomicBool) -> io::Result<()> {
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
    fn definition_file(&self) -> io::Result<Option<(File, FileFingerprint, Vec<u8>)>> {
        let mut file = match self
            .root
            .open_leaf_classified(std::ffi::OsStr::new(&self.authority.unit))
        {
            crate::fs::LeafOpenResult::Opened(file) => file,
            crate::fs::LeafOpenResult::MissingSettled => return Ok(None),
            crate::fs::LeafOpenResult::FailedSettled(error) => return Err(error),
            crate::fs::LeafOpenResult::Unsettled(owner) => return Err(io::Error::other(owner)),
        };
        let before = fingerprint(&file)?;
        if before.len > 65536 {
            return Err(invalid("service definition exceeds 65536 bytes"));
        }
        let mut bytes = Vec::new();
        (&mut file).take(65537).read_to_end(&mut bytes)?;
        if bytes.len() > 65536 || fingerprint(&file)? != before {
            return Err(invalid("service definition changed during read"));
        }
        Ok(Some((file, before, bytes)))
    }
    fn unit_file_state(
        &self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Option<String>> {
        let mut request = self.bus.method(
            &self.authority.manager_unique_name,
            MANAGER_PATH,
            MANAGER_IFACE,
            "ListUnitFilesByPatterns",
        )?;
        request.append_array(&[])?;
        request.append_array(&[&self.authority.unit])?;
        let rpc = self.bus.call(&request, remaining(deadline, cancel)?)?;
        let mut reply = self.rpc_wait(&rpc, deadline, cancel)?;
        reply.require_enter(b'a', c"(ss)")?;
        let mut state = None;
        while reply.enter(b'r', c"ss")? {
            let path = reply.string(b's')?;
            let value = reply.string(b's')?;
            reply.exit()?;
            if Path::new(&path) != self.authority.definition_root.join(&self.authority.unit)
                || state.is_some()
            {
                return Err(invalid("unit file has competing origin"));
            }
            state = Some(value);
        }
        reply.exit()?;
        Ok(state)
    }
    fn loaded(
        &self,
        path: &str,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<LoadedService> {
        let sp = |interface, name| self.string_property(path, interface, name, deadline, cancel);
        let asp = |name| self.strings_property(path, UNIT_IFACE, name, deadline, cancel);
        let unit = sp(UNIT_IFACE, "Id")?;
        let names = asp("Names")?;
        let load_state = sp(UNIT_IFACE, "LoadState")?;
        let active_state = sp(UNIT_IFACE, "ActiveState")?;
        let sub_state = sp(UNIT_IFACE, "SubState")?;
        let fragment_path = sp(UNIT_IFACE, "FragmentPath")?;
        let drop_in_paths = asp("DropInPaths")?;
        let invocation_id = self
            .property(path, UNIT_IFACE, "InvocationID", c"ay", deadline, cancel)?
            .bytes()?;
        let need_daemon_reload = self
            .property(path, UNIT_IFACE, "NeedDaemonReload", c"b", deadline, cancel)?
            .u32(b'b')?
            != 0;
        let service_type = sp(SERVICE_IFACE, "Type")?;
        let restart = sp(SERVICE_IFACE, "Restart")?;
        let exec = self
            .property(
                path,
                SERVICE_IFACE,
                "ExecStart",
                c"a(sasbttttuii)",
                deadline,
                cancel,
            )?
            .exec()?;
        let environment =
            self.strings_property(path, SERVICE_IFACE, "Environment", deadline, cancel)?;
        let working_directory = sp(SERVICE_IFACE, "WorkingDirectory")?;
        let main_pid = self
            .property(path, SERVICE_IFACE, "MainPID", c"u", deadline, cancel)?
            .u32(b'u')?;
        let result = sp(SERVICE_IFACE, "Result")?;
        let definition = match exec {
            Some((executable, arguments, ignore)) => {
                if ignore {
                    return Err(invalid("ExecStart ignore-failure unsupported"));
                }
                let definition = SystemdDefinition {
                    executable,
                    arguments,
                    environment,
                    working_directory,
                };
                definition.validate()?;
                Some(definition)
            }
            None => None,
        };
        Ok(LoadedService {
            unit,
            names,
            load_state,
            active_state,
            sub_state,
            fragment_path,
            drop_in_paths,
            invocation_id,
            need_daemon_reload,
            service_type,
            restart,
            definition,
            main_pid,
            result,
        })
    }
    pub fn inspect(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<ServiceObservation> {
        self.verify_context(deadline, cancel)?;
        let result = (|| {
            let definition_bytes = match self.definition_file()? {
                Some((_, _, bytes)) => {
                    validate_definition_source(&bytes)?;
                    Some(bytes)
                }
                None => None,
            };
            let definition_present = definition_bytes.is_some();
            let unit_file_state = self.unit_file_state(deadline, cancel)?;
            // LoadUnit, not GetUnit: a real manager garbage-collects an inactive,
            // unreferenced unit right after the SUT's own LoadUnit (CI5 journal:
            // "Collecting." 0.6 ms later), so GetUnit would report NoSuchUnit for
            // an installed registration. LoadUnit reads only the exact unit name
            // from the original closed unit path; an absent definition yields a
            // not-found unit (or NoSuchUnit) and is still observed as absent.
            // Limit: after collection this inspection itself performs the load
            // (NeedDaemonReload then reads false), so it proves the manager's
            // current reading of the exact definition, not that the SUT reloaded.
            let mut message = self.bus.method(
                &self.authority.manager_unique_name,
                MANAGER_PATH,
                MANAGER_IFACE,
                "LoadUnit",
            )?;
            message.append(&self.authority.unit)?;
            let rpc = self.bus.call(&message, remaining(deadline, cancel)?)?;
            let reply = loop {
                remaining(deadline, cancel)?;
                if let Some(reply) = rpc.reply() {
                    break reply;
                }
                self.bus.process()?;
                self.bus.poll(25)?;
            };
            let loaded = match reply.error()? {
                Some(name) if name == "org.freedesktop.systemd1.NoSuchUnit" => None,
                Some(name) => return Err(io::Error::other(name)),
                None => {
                    let mut reply = reply;
                    let path = reply.string(b'o')?;
                    Some(self.loaded(&path, deadline, cancel)?)
                }
            };
            if let Some(loaded) = &loaded {
                self.check_origin(loaded)?;
            }
            Ok(ServiceObservation {
                definition_present,
                definition_bytes,
                unit_file_state,
                loaded,
            })
        })();
        if result.is_err() {
            self.revoked = true;
        }
        self.verify_context(deadline, cancel)?;
        result
    }
    fn check_origin(&self, loaded: &LoadedService) -> io::Result<()> {
        // A not-found cached unit may remain visible after SUT removal. It cannot
        // grant action authority, but preserves independent runtime observations.
        if loaded.unit != self.authority.unit
            || loaded.names != [self.authority.unit.clone()]
            || !loaded.drop_in_paths.is_empty()
            || (!loaded.fragment_path.is_empty()
                && Path::new(&loaded.fragment_path)
                    != self.authority.definition_root.join(&self.authority.unit))
        {
            return Err(invalid(
                "unit origin, aliases or drop-ins outside original context",
            ));
        }
        Ok(())
    }
    pub fn qualify_registration(
        &mut self,
        definition: &SystemdDefinition,
        install_receipt: String,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<RegistrationOwner> {
        if self.revoked {
            return Err(invalid("original systemd authority permanently revoked"));
        }
        definition.validate()?;
        if install_receipt.is_empty() || install_receipt.len() > 4096 {
            return Err(invalid("missing original accepted SUT install receipt"));
        }
        let observation = self.inspect(deadline, cancel)?;
        let Some(loaded) = observation.loaded else {
            return Err(invalid(
                "installed registration not loadable by the original manager",
            ));
        };
        if let Err(error) = self.check_definition(&loaded, definition) {
            self.revoked = true;
            return Err(error);
        }
        if !observation.definition_present || observation.unit_file_state.is_none() {
            return Err(invalid("registration definition is absent"));
        }
        let Some((file, fingerprint, definition_bytes)) = self.definition_file()? else {
            return Err(invalid("registration disappeared"));
        };
        if !matches!(
            observation.unit_file_state.as_deref(),
            Some("static" | "disabled")
        ) {
            self.revoked = true;
            return Err(invalid(
                "automatic activation or unsupported unit-file state",
            ));
        }
        if let Err(error) = validate_definition_source(&definition_bytes) {
            self.revoked = true;
            return Err(error);
        }
        Ok(RegistrationOwner {
            _file: file,
            prior_files: vec![],
            transition_receipts: vec![],
            fingerprint,
            definition_bytes,
            definition: definition.clone(),
            manager_instance: self.instance,
            install_receipt,
            revoked: false,
        })
    }
    /// Admit an explicit completed SUT definition transition on the retained owner.
    /// The old file owners remain held; neither a name lookup nor a restored
    /// definition can revive a previously revoked registration.
    pub fn admit_configuration(
        &mut self,
        owner: &mut RegistrationOwner,
        definition: &SystemdDefinition,
        expected_bytes: &[u8],
        sut_receipt: String,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if self.revoked || owner.revoked || owner.manager_instance != self.instance {
            owner.revoked = true;
            self.revoked = true;
            return Err(invalid("original registration permanently revoked"));
        }
        if self.pending.is_some()
            || owner.transition_receipts.len() >= 128
            || owner.install_receipt == sut_receipt
            || owner.transition_receipts.contains(&sut_receipt)
            || sut_receipt.is_empty()
            || sut_receipt.len() > 4096
            || sut_receipt.chars().any(char::is_control)
        {
            return Err(invalid("original systemd SUT transition unavailable"));
        }
        let result = (|| {
            definition.validate()?;
            self.verify_context(deadline, cancel)?;
            let observation = self.inspect(deadline, cancel)?;
            let loaded = observation
                .loaded
                .as_ref()
                .ok_or_else(|| invalid("SUT transition is not loaded"))?;
            self.check_definition(loaded, definition)?;
            let (file, fingerprint, bytes) = self
                .definition_file()?
                .ok_or_else(|| invalid("SUT transition definition missing"))?;
            if bytes != expected_bytes
                || observation.definition_bytes.as_ref() != Some(&bytes)
                || !matches!(
                    observation.unit_file_state.as_deref(),
                    Some("static" | "disabled")
                )
            {
                return Err(invalid(
                    "SUT transition differs from exact admitted definition",
                ));
            }
            validate_definition_source(&bytes)?;
            Ok((file, fingerprint, bytes))
        })();
        match result {
            Ok((file, fingerprint, bytes)) => {
                owner.record_transition(file, fingerprint, bytes, definition, sut_receipt)
            }
            Err(error) => {
                owner.revoked = true;
                self.revoked = true;
                Err(error)
            }
        }
    }
    fn check_definition(
        &self,
        loaded: &LoadedService,
        expected: &SystemdDefinition,
    ) -> io::Result<()> {
        self.check_origin(loaded)?;
        if loaded.load_state != "loaded"
            || loaded.fragment_path.is_empty()
            || loaded.need_daemon_reload
            || loaded.service_type != "simple"
            || loaded.restart != "no"
            || loaded.definition.as_ref() != Some(expected)
        {
            return Err(invalid(
                "loaded configuration does not match qualified definition",
            ));
        }
        Ok(())
    }
    pub fn verify_registration(
        &mut self,
        owner: &mut RegistrationOwner,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.verify_owner(owner, deadline, cancel)
    }
    fn verify_owner(
        &mut self,
        owner: &mut RegistrationOwner,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if self.revoked || owner.revoked || owner.manager_instance != self.instance {
            owner.revoked = true;
            self.revoked = true;
            return Err(invalid("registration owner not original to manager"));
        }
        let result = (|| {
            let observation = self.inspect(deadline, cancel)?;
            let Some(loaded) = observation.loaded else {
                return Err(invalid("original registration no longer loaded"));
            };
            self.check_definition(&loaded, &owner.definition)?;
            let Some((_, current, bytes)) = self.definition_file()? else {
                return Err(invalid("original definition missing"));
            };
            if current != owner.fingerprint || bytes != owner.definition_bytes {
                return Err(invalid("original definition replaced or modified"));
            }
            Ok(())
        })();
        if result.is_err() {
            owner.revoked = true;
            self.revoked = true;
        }
        result
    }
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
    fn advance_action(&mut self) -> io::Result<Option<OwnedActionProgress>> {
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

fn validate_definition_source(bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > 65536 || bytes.contains(&0) {
        return Err(invalid("unbounded or NUL service definition"));
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| invalid("service definition is not UTF-8"))?;
    let mut section = "";
    let mut seen = std::collections::BTreeSet::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if matches!(line, "[Unit]" | "[Service]") {
            section = line;
            continue;
        }
        if line.ends_with('\\') || line.starts_with('[') {
            return Err(invalid("unsupported service section or continuation"));
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(invalid("invalid service directive"));
        };
        let (key, value) = (key.trim(), value.trim());
        let allowed = match (section, key) {
            ("[Unit]", "Description") => true,
            ("[Unit]", "DefaultDependencies") => value == "no",
            ("[Service]", "Type") => value == "simple",
            ("[Service]", "Restart") => value == "no",
            ("[Service]", "ExecStart" | "WorkingDirectory") => {
                !value.is_empty()
                    && !value.contains(['$', '%'])
                    && (value.starts_with('/') || value.starts_with("\"/"))
            }
            ("[Service]", "Environment") => !value.contains('%'),
            _ => false,
        };
        if !allowed || (key != "Environment" && !seen.insert((section, key))) {
            return Err(invalid("unsupported or duplicate service directive"));
        }
    }
    if !seen.contains(&("[Service]", "ExecStart")) {
        return Err(invalid("missing exact service command"));
    }
    Ok(())
}
#[cfg(test)]
mod definition_model_tests {
    use super::*;
    #[test]
    fn model_refuses_hidden_exec_and_dependency_configuration() {
        let base = "[Unit]\nDescription=Fixture\nDefaultDependencies=no\n[Service]\nType=simple\nExecStart=/fixture/service\nWorkingDirectory=/fixture\nRestart=no\n";
        assert!(validate_definition_source(base.as_bytes()).is_ok());
        for extra in [
            "ExecStartPre=/unadmitted",
            "EnvironmentFile=/unadmitted",
            "Restart=always",
            "User=other",
            "Sockets=other.socket",
            "Type=forking",
            "ExecStart=/second",
            "[Install]\nWantedBy=default.target",
            "[Unit]\nRequires=unadmitted.service",
        ] {
            assert!(
                validate_definition_source(format!("{base}{extra}\n").as_bytes()).is_err(),
                "accepted {extra}"
            );
        }
    }
    #[test]
    fn fixture_renderer_quoted_absolute_command_is_supported_without_prefix_escape() {
        let definition=b"[Unit]\nDescription=Theseus fixture\nDefaultDependencies=no\n[Service]\nType=simple\nRestart=no\nExecStart=\"/fixture/service\" \"service\" \"/fixture/ready.sock\"\nWorkingDirectory=/fixture\nEnvironment=\"VALUE=fixture value\"\n";
        assert!(validate_definition_source(definition).is_ok());
        for prefix in ["+", "!", "@", "-", ":"] {
            let text = format!("[Service]\nExecStart={prefix}/fixture/service\n");
            assert!(validate_definition_source(text.as_bytes()).is_err());
        }
    }
    #[test]
    fn cooperative_deadline_and_cancel_have_no_default_timeout_fallback() {
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            remaining(Instant::now() + Duration::from_secs(1), &cancelled)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        let cancel = AtomicBool::new(false);
        assert_eq!(
            remaining(Instant::now() - Duration::from_secs(1), &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(remaining(Instant::now() + Duration::from_secs(31), &cancel).is_err());
        assert!(remaining(Instant::now() + Duration::from_secs(1), &cancel).unwrap() > 0);
    }
}

#[cfg(test)]
mod sut_transition_owner_tests {
    use super::*;
    #[test]
    fn sut_transition_keeps_original_files_and_never_restores_revoked_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("original");
        std::fs::write(&path, b"base").unwrap();
        let file = File::open(&path).unwrap();
        let fp = crate::fs::fingerprint(&file).unwrap();
        let definition = SystemdDefinition {
            executable: "/fixture/base".into(),
            arguments: vec![],
            environment: vec![],
            working_directory: "/fixture".into(),
        };
        let mut owner = RegistrationOwner {
            _file: file,
            prior_files: vec![],
            transition_receipts: vec![],
            fingerprint: fp.clone(),
            definition_bytes: b"base".to_vec(),
            definition: definition.clone(),
            manager_instance: [1; 32],
            install_receipt: "original-install".into(),
            revoked: false,
        };
        let next = dir.path().join("target");
        std::fs::write(&next, b"target").unwrap();
        let replacement = File::open(&next).unwrap();
        let replacement_fp = crate::fs::fingerprint(&replacement).unwrap();
        owner
            .record_transition(
                replacement,
                replacement_fp,
                b"target".to_vec(),
                &definition,
                "accepted-upgrade".into(),
            )
            .unwrap();
        assert_eq!(owner.install_receipt(), "original-install");
        assert_eq!(owner.prior_files.len(), 1);
        assert_eq!(crate::fs::fingerprint(&owner.prior_files[0]).unwrap(), fp);
        assert_eq!(owner.definition_bytes(), b"target");
        owner.revoked = true;
        let original = File::open(&path).unwrap();
        assert!(
            owner
                .record_transition(
                    original,
                    fp,
                    b"base".to_vec(),
                    &definition,
                    "accepted-reinstall".into()
                )
                .is_err()
        );
        assert!(owner.is_revoked());
        assert_eq!(owner.definition_bytes(), b"target");
        assert_eq!(owner.prior_files.len(), 1);
    }
}
