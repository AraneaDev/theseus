//! Admitted literal desktop-entry definition and its closed parser.
use super::{MAX_DEFINITION, invalid};
use std::{collections::BTreeMap, io, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiteralCommand {
    pub executable: String,
    pub arguments: Vec<String>,
}
/// Independently admitted manifest commitment, never populated from the entry.
#[derive(Clone, Debug)]
pub struct ArtifactSlot {
    pub root: PathBuf,
    pub relative: String,
    pub length: u64,
    pub sha256: [u8; 32],
}
#[derive(Clone, Debug)]
pub struct AdmittedDefinition {
    pub entry: ArtifactSlot,
    pub executable: ArtifactSlot,
    pub command: LiteralCommand,
}

/// Closed subset: no quoting, escaping or field expansion. The native parser
/// later reads these same bytes, and its executable/commandline must agree.
pub(super) fn parse_entry(bytes: &[u8], expected: &LiteralCommand) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_DEFINITION as usize || bytes.contains(&0) {
        return Err(invalid("desktop entry outside byte bound"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("desktop entry is not UTF-8"))?;
    let mut fields = BTreeMap::new();
    let mut group = false;
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[Desktop Entry]" && !group && fields.is_empty() {
            group = true;
            continue;
        }
        if !group || line.starts_with('[') || line.len() > 8192 || line.contains('\r') {
            return Err(invalid("unsupported desktop group or line"));
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| invalid("invalid desktop field"))?;
        if !matches!(
            key,
            "Type" | "Name" | "Exec" | "Terminal" | "DBusActivatable"
        ) || fields.insert(key, value).is_some()
        {
            return Err(invalid("unknown or duplicate desktop field"));
        }
    }
    if fields.len() != 5
        || fields.get("Type") != Some(&"Application")
        || fields.get("Terminal") != Some(&"false")
        || fields.get("DBusActivatable") != Some(&"false")
        || !fields.get("Name").is_some_and(|v| {
            !v.is_empty() && v.len() <= 256 && !v.chars().any(char::is_control) && !v.contains('\\')
        })
    {
        return Err(invalid("unsupported desktop application policy"));
    }
    validate_command(expected)?;
    let exec = fields["Exec"];
    let tokens: Vec<_> = exec.split(' ').collect();
    if tokens.len() != expected.arguments.len() + 1
        || tokens.first() != Some(&expected.executable.as_str())
        || tokens[1..]
            .iter()
            .copied()
            .ne(expected.arguments.iter().map(String::as_str))
        || tokens.iter().any(|v| !literal(v))
    {
        return Err(invalid(
            "desktop Exec differs from admitted literal command",
        ));
    }
    Ok(())
}
fn literal(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.contains("://")
        && value
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || b"/_.-+=,@".contains(&v))
}
fn validate_command(command: &LiteralCommand) -> io::Result<()> {
    if !absolute(&command.executable)
        || !literal(&command.executable)
        || command.arguments.len() > 64
        || command.arguments.iter().any(|arg| !literal(arg))
        || command.arguments.iter().map(String::len).sum::<usize>() > 8192
    {
        return Err(invalid(
            "command must contain one absolute executable and bounded literal argv",
        ));
    }
    Ok(())
}
pub(super) fn absolute(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 2048
        && !value.as_bytes().contains(&0)
        && (value == "/"
            || !value[1..]
                .split('/')
                .any(|v| v.is_empty() || matches!(v, "." | "..")))
}
