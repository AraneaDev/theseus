//! Closed Windows shortcut admission and retained original shell process lifetime.
//! Durable original broker acceptance is a caller prerequisite; this adapter
//! does not discover sessions, install shortcuts, elevate or attach by PID.
use std::io;
#[cfg(windows)]
mod com;
#[cfg(any(test, feature = "desktop-test-support"))]
pub mod fixture;
#[cfg(windows)]
mod native;
#[cfg(any(windows, test))]
mod probe;
#[cfg(windows)]
mod retained;
#[cfg(windows)]
pub use com::{AdmittedShortcut, ComApartment, NativeCode, NativeError, ShortcutRetireFailure};
#[cfg(windows)]
pub use native::{LifetimeObservation, NativeActivation, NativeLifetime, NativeStartFailure};
#[cfg(windows)]
pub use retained::RetainedShortcut;
#[derive(Clone, Debug)]
pub struct ArtifactCommitment {
    pub length: u64,
    pub sha256: [u8; 32],
}
#[derive(Clone, Debug)]
pub struct ShortcutProfile {
    pub root: String,
    pub entry: String,
    pub executable: String,
    pub arguments: Vec<String>,
    pub entry_artifact: ArtifactCommitment,
    pub executable_artifact: ArtifactCommitment,
}
#[derive(Clone, Debug)]
pub struct ShortcutMetadata {
    pub target: String,
    pub arguments: String,
    pub working_directory: String,
    pub flags: u32,
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
impl ShortcutProfile {
    pub fn validate(&self) -> io::Result<()> {
        if !absolute_root(&self.root)
            || !leaf(&self.entry)
            || !self.entry.ends_with(".lnk")
            || self.entry.len() <= 4
            || !leaf(&self.executable)
            || !self.executable.ends_with(".exe")
            || self.executable.len() <= 4
            || self.entry_artifact.length < 76
            || self.entry_artifact.length > 65536
            || self.executable_artifact.length == 0
            || self.executable_artifact.length > 64 * 1024 * 1024
            || self.entry_artifact.sha256 == [0; 32]
            || self.executable_artifact.sha256 == [0; 32]
            || self.arguments.len() > 64
            || self
                .arguments
                .iter()
                .any(|a| a.len() > 1024 || a.as_bytes().contains(&0))
            || command_line(&self.arguments).encode_utf16().count() > 16384
        {
            return Err(invalid("unsupported closed shortcut profile"));
        }
        Ok(())
    }
    pub fn admit_metadata(&self, metadata: &ShortcutMetadata) -> io::Result<()> {
        self.validate()?;
        // Exact comparisons; each refusal names only the differing field.
        if metadata.target != format!("{}\\{}", self.root, self.executable) {
            return Err(invalid(
                "native shortcut target differs from exact admitted target",
            ));
        }
        if metadata.arguments != command_line(&self.arguments) {
            return Err(invalid(
                "native shortcut arguments differ from exact admitted argv",
            ));
        }
        if metadata.working_directory != self.root {
            return Err(invalid(
                "native shortcut working directory differs from exact admitted cwd",
            ));
        }
        if metadata.flags & !0xb2 != 0 || metadata.flags & 0x82 != 0x82 {
            return Err(invalid(
                "native shortcut flags differ from exact admitted closed flags",
            ));
        }
        Ok(())
    }
}
fn leaf(v: &str) -> bool {
    if v.is_empty()
        || v.len() > 128
        || v.ends_with('.')
        || v.ends_with(' ')
        || !v
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        || matches!(v, "." | "..")
    {
        return false;
    }
    let stem = v.split('.').next().unwrap_or_default().to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) && !(stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}
fn absolute_root(v: &str) -> bool {
    let bytes = v.as_bytes();
    bytes.len() > 3
        && bytes.len() <= 1024
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes[2] == b'\\'
        && v[3..].split('\\').all(leaf)
}
/// Unknown/absent manifests and all elevation/UIAccess requests refuse.
pub fn admit_run_level(level: u32, ui_access: bool) -> io::Result<()> {
    if level != 1 || ui_access {
        return Err(invalid("explicit asInvoker/no-uiAccess manifest required"));
    }
    Ok(())
}
/// Exact Microsoft CRT argv encoding; no shell metacharacter interpretation.
pub fn command_line(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|arg| {
            if !arg.is_empty() && !arg.chars().any(|c| c.is_whitespace() || c == '"') {
                return arg.clone();
            }
            let mut out = String::from("\"");
            let mut slashes = 0;
            for c in arg.chars() {
                if c == '\\' {
                    slashes += 1;
                    continue;
                }
                if c == '"' {
                    out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                    out.push('"');
                } else {
                    out.extend(std::iter::repeat_n('\\', slashes));
                    out.push(c);
                }
                slashes = 0;
            }
            out.extend(std::iter::repeat_n('\\', slashes * 2));
            out.push('"');
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}
/// Structural restriction supplementing native COM parsing. Reject PIDLs,
/// runas/environment/Darwin/DDE-related extensions and all ExtraData blocks.
pub fn validate_link_bytes(bytes: &[u8]) -> io::Result<u32> {
    fn u32at(b: &[u8], i: usize) -> io::Result<u32> {
        Ok(u32::from_le_bytes(
            b.get(i..i + 4)
                .ok_or_else(|| invalid("truncated shortcut"))?
                .try_into()
                .unwrap(),
        ))
    }
    if bytes.len() < 80
        || bytes.len() > 65536
        || u32at(bytes, 0)? != 76
        || bytes[4..20] != [1, 20, 2, 0, 0, 0, 0, 0, 192, 0, 0, 0, 0, 0, 0, 70]
    {
        return Err(invalid("unsupported shell link header"));
    }
    let flags = u32at(bytes, 20)?;
    if flags & !0xb2 != 0
        || flags & 0x82 != 0x82
        || u32at(bytes, 60)? != 1
        || bytes[64..76].iter().any(|b| *b != 0)
    {
        return Err(invalid("unsupported shortcut flags/show/hotkey"));
    }
    let info = u32at(bytes, 76)? as usize;
    if info < 28
        || info.checked_add(76).is_none_or(|end| end > bytes.len())
        || u32at(bytes, 84)? != 1
    {
        return Err(invalid("only local LinkInfo is admitted"));
    }
    let mut offset = 76 + info;
    for flag in [0x10, 0x20] {
        if flags & flag != 0 {
            let count = u16::from_le_bytes(
                bytes
                    .get(offset..offset + 2)
                    .ok_or_else(|| invalid("truncated shortcut string"))?
                    .try_into()
                    .unwrap(),
            ) as usize;
            offset = offset
                .checked_add(2 + count * 2)
                .ok_or_else(|| invalid("shortcut string overflow"))?;
            if offset > bytes.len() {
                return Err(invalid("truncated shortcut string"));
            }
        }
    }
    if offset + 4 != bytes.len() || u32at(bytes, offset)? != 0 {
        return Err(invalid("shortcut ExtraData is unsupported"));
    }
    Ok(flags)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationEffect {
    Pending,
    Unavailable,
    Acknowledged,
    Unknown,
}
/// Pure diagnostic projection. It cannot construct any native owner.
pub struct ActivationProgress {
    effect: ActivationEffect,
    returned: bool,
    handle: bool,
    fresh: bool,
    exited: bool,
}
impl ActivationProgress {
    pub fn pending() -> Self {
        Self {
            effect: ActivationEffect::Pending,
            returned: false,
            handle: false,
            fresh: false,
            exited: false,
        }
    }
    pub fn native_return(&mut self, handle: bool) -> io::Result<()> {
        if self.returned {
            return Err(invalid("duplicate native return"));
        }
        self.returned = true;
        self.handle = handle;
        self.effect = ActivationEffect::Unavailable;
        Ok(())
    }
    pub fn authenticate_new_instance(&mut self, matches: bool) -> io::Result<()> {
        if !self.returned || !self.handle || self.effect == ActivationEffect::Unknown {
            return Err(invalid("no original process object"));
        }
        self.fresh = matches;
        self.effect = if matches {
            ActivationEffect::Acknowledged
        } else {
            ActivationEffect::Unknown
        };
        Ok(())
    }
    pub fn mark_deadline_or_cancel(&mut self) {}
    pub fn effect(&self) -> ActivationEffect {
        self.effect
    }
    pub fn new_instance_proved(&self) -> bool {
        self.fresh
    }
    pub fn owned_primary_available(&self) -> bool {
        self.handle
    }
    pub fn descendants_owned(&self) -> bool {
        false
    }
    pub fn primary_exited(&self) -> bool {
        self.exited
    }
    pub fn observe_primary_exit(&mut self) {
        self.exited = true;
    }
}
#[cfg(test)]
mod tests;
