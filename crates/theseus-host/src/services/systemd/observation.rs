//! Unit-file and loaded-unit observation.
use super::sdk::invalid;
use super::{
    LoadedService, MANAGER_IFACE, MANAGER_PATH, SERVICE_IFACE, ServiceObservation,
    SystemdDefinition, SystemdManager, UNIT_IFACE, remaining, validate_definition_source,
};
use crate::fs::{FileFingerprint, fingerprint};
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
    sync::atomic::AtomicBool,
    time::Instant,
};

impl SystemdManager {
    pub(super) fn definition_file(&self) -> io::Result<Option<(File, FileFingerprint, Vec<u8>)>> {
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
    pub(super) fn unit_file_state(
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
    pub(super) fn loaded(
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
    pub(super) fn check_origin(&self, loaded: &LoadedService) -> io::Result<()> {
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
}
