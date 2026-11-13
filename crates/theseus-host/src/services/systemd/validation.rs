//! Authority and unit-definition validation.
use super::sdk::invalid;
use super::{SystemdAuthority, SystemdDefinition};
use std::{io, path::Path};

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

pub(super) fn validate_definition_source(bytes: &[u8]) -> io::Result<()> {
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
            ("[Service]", "ExecStart") => {
                !value.is_empty()
                    && !value.contains(['$', '%'])
                    && (value.starts_with('/') || value.starts_with("\"/"))
            }
            // A real manager reads path settings literally (quotes are never
            // stripped), so only an unquoted absolute literal is admitted.
            ("[Service]", "WorkingDirectory") => {
                value.starts_with('/') && !value.contains(['$', '%', '"', '\''])
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
