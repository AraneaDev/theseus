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
    pub layout: LinkLayout,
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
        let arguments_flag = if self.arguments.is_empty() {
            0
        } else {
            LINK_ARGUMENTS_FLAG
        };
        if metadata.layout.flags != LINK_REQUIRED_FLAGS | arguments_flag {
            return Err(invalid(
                "native shortcut flags differ from exact admitted closed flags",
            ));
        }
        // The Shell's resolution fallbacks must name the same exact target.
        if metadata.layout.local_base_path != metadata.target
            || metadata.layout.relative_path != format!(".\\{}", self.executable)
        {
            return Err(invalid(
                "native shortcut fallback paths differ from exact admitted target",
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
/// LinkFlags of the closed Shell-authored profile, exactly as
/// IShellLinkW::SetPath/SetArguments/SetWorkingDirectory + IPersistFile::Save
/// write them on Windows (CI8 windows-2025): HasLinkTargetIDList (0x1),
/// HasLinkInfo (0x2), HasRelativePath (0x8), HasWorkingDir (0x10), IsUnicode
/// (0x80), plus HasArguments (0x20) exactly when argv is non-empty.
pub const LINK_REQUIRED_FLAGS: u32 = 0x9b;
pub const LINK_ARGUMENTS_FLAG: u32 = 0x20;
/// Structural facts of an admitted closed shortcut, compared exactly against
/// the profile together with the native COM-reported target/argv/cwd.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkLayout {
    pub flags: u32,
    /// LinkInfo LocalBasePath (the Shell's resolution fallback target).
    pub local_base_path: String,
    /// StringData RELATIVE_PATH (the Shell's relative resolution fallback).
    pub relative_path: String,
}
const SPECIAL_FOLDER_BLOCK: u32 = 0xa000_0005;
const KNOWN_FOLDER_BLOCK: u32 = 0xa000_000b;
const TRACKER_BLOCK: u32 = 0xa000_0003;
const PROPERTY_STORE_BLOCK: u32 = 0xa000_0009;
/// The only property sets observed in Shell-authored shortcuts: the owner SID
/// ({46588AE2-4CBC-4338-BBFC-139326986DCE}, 4, VT_LPWSTR) and System.VolumeId
/// ({446D16B1-8DAD-4870-A748-402EA43D788C}, 104, VT_CLSID).
const OWNER_SID_FMTID: [u8; 16] = [
    0xe2, 0x8a, 0x58, 0x46, 0xbc, 0x4c, 0x38, 0x43, 0xbb, 0xfc, 0x13, 0x93, 0x26, 0x98, 0x6d, 0xce,
];
const VOLUME_ID_FMTID: [u8; 16] = [
    0xb1, 0x16, 0x6d, 0x44, 0xad, 0x8d, 0x70, 0x48, 0xa7, 0x48, 0x40, 0x2e, 0xa4, 0x3d, 0x78, 0x8c,
];
fn u16_at(b: &[u8], i: usize) -> io::Result<usize> {
    Ok(u16::from_le_bytes(
        b.get(
            i..i.checked_add(2)
                .ok_or_else(|| invalid("shortcut offset overflow"))?,
        )
        .ok_or_else(|| invalid("truncated shortcut"))?
        .try_into()
        .unwrap(),
    ) as usize)
}
fn u32_at(b: &[u8], i: usize) -> io::Result<u32> {
    Ok(u32::from_le_bytes(
        b.get(
            i..i.checked_add(4)
                .ok_or_else(|| invalid("shortcut offset overflow"))?,
        )
        .ok_or_else(|| invalid("truncated shortcut"))?
        .try_into()
        .unwrap(),
    ))
}
/// Size-checked opaque LinkTargetIDList. Its meaning is proven separately by
/// the exact COM GetPath match; here only bounded, exact framing is admitted.
fn validate_id_list(bytes: &[u8], offset: usize) -> io::Result<usize> {
    let size = u16_at(bytes, offset)?;
    let list = bytes
        .get(offset + 2..offset + 2 + size)
        .ok_or_else(|| invalid("truncated shortcut IDList"))?;
    let (mut at, mut items) = (0, 0);
    loop {
        let item = u16_at(list, at)?;
        if item == 0 {
            break;
        }
        items += 1;
        if item < 3 || items > 32 {
            return Err(invalid("unsupported shortcut IDList item"));
        }
        at += item;
    }
    if items == 0 || at + 2 != size {
        return Err(invalid("unsupported shortcut IDList framing"));
    }
    Ok(size)
}
/// Local fixed-volume LinkInfo without Unicode offsets or network data.
fn validate_link_info(bytes: &[u8], offset: usize) -> io::Result<(usize, String)> {
    let size = u32_at(bytes, offset)? as usize;
    let info = bytes
        .get(
            offset
                ..offset
                    .checked_add(size)
                    .ok_or_else(|| invalid("LinkInfo overflow"))?,
        )
        .ok_or_else(|| invalid("truncated LinkInfo"))?;
    if !(28..=4096).contains(&size)
        || u32_at(info, 4)? != 28
        || u32_at(info, 8)? != 1
        || u32_at(info, 12)? != 28
        || u32_at(info, 20)? != 0
    {
        return Err(invalid("only local LinkInfo is admitted"));
    }
    let volume = 28usize;
    let volume_size = u32_at(info, volume)? as usize;
    let label = u32_at(info, volume + 12)? as usize;
    let base = u32_at(info, 16)? as usize;
    let suffix = u32_at(info, 24)? as usize;
    if !(17..=64).contains(&volume_size)
        || volume + volume_size != base
        || u32_at(info, volume + 4)? != 3
        || label != 16
        || info.get(volume + volume_size - 1) != Some(&0)
        || info[volume + label..volume + volume_size - 1].contains(&0)
    {
        return Err(invalid("only local fixed-volume LinkInfo is admitted"));
    }
    let terminated = |at: usize| -> io::Result<&[u8]> {
        let rest = info
            .get(at..)
            .ok_or_else(|| invalid("truncated LinkInfo"))?;
        let end = rest
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| invalid("unterminated LinkInfo path"))?;
        Ok(&rest[..end])
    };
    let path = terminated(base)?;
    if base + path.len() + 1 != suffix
        || !terminated(suffix)?.is_empty()
        || suffix + 1 != size
        || !path.is_ascii()
    {
        return Err(invalid("unsupported LinkInfo path layout"));
    }
    Ok((size, String::from_utf8(path.to_vec()).unwrap()))
}
fn validate_property_store(data: &[u8]) -> io::Result<()> {
    let (mut at, mut seen) = (0, Vec::new());
    loop {
        let size = u32_at(data, at)? as usize;
        if size == 0 {
            break;
        }
        let storage = data
            .get(
                at..at
                    .checked_add(size)
                    .ok_or_else(|| invalid("property overflow"))?,
            )
            .ok_or_else(|| invalid("truncated property storage"))?;
        // Serialized storage: size, "1SPS", FMTID, one integer-named value,
        // value terminator. Exactly one known value per known property set.
        let fmtid: [u8; 16] = storage
            .get(8..24)
            .ok_or_else(|| invalid("truncated property storage"))?
            .try_into()
            .unwrap();
        let value_size = u32_at(storage, 24)? as usize;
        let id = u32_at(storage, 28)?;
        let kind = u32_at(storage, 33)?;
        let value = storage
            .get(37..24 + value_size)
            .ok_or_else(|| invalid("truncated property value"))?;
        let valid = u32_at(storage, 4)? == 0x5350_5331
            && storage.get(32) == Some(&0)
            && value_size >= 13
            && 24 + value_size + 4 == size
            && u32_at(storage, 24 + value_size)? == 0
            && !seen.contains(&fmtid)
            && match (fmtid, id, kind) {
                (OWNER_SID_FMTID, 4, 0x1f) => {
                    let count = u32_at(value, 0)? as usize;
                    let text = value.get(4..).unwrap_or_default();
                    (2..=184).contains(&count)
                        && text.len() >= count * 2
                        && text[..count * 2].chunks(2).enumerate().all(|(i, c)| {
                            let unit = u16::from_le_bytes([c[0], c[1]]);
                            if i + 1 == count {
                                unit == 0
                            } else {
                                unit < 0x80
                                    && (unit as u8 == b'-' || (unit as u8).is_ascii_alphanumeric())
                            }
                        })
                        && text[count * 2..].iter().all(|b| *b == 0)
                        && text.len() - count * 2 < 4
                }
                (VOLUME_ID_FMTID, 104, 0x48) => value.len() == 16,
                _ => false,
            };
        if !valid {
            return Err(invalid("unsupported shortcut property store"));
        }
        seen.push(fmtid);
        at += size;
    }
    if at + 4 != data.len() {
        return Err(invalid("unsupported shortcut property store framing"));
    }
    Ok(())
}
/// Closed structural profile of a Shell-authored local shortcut (derived from
/// the exact bytes IPersistFile::Save wrote on windows-2025, CI8). Anything
/// else refuses: other LinkFlags (name, icon, environment, RunAs, Darwin,
/// ...), non-default show/hotkey/icon, network LinkInfo, and every ExtraData
/// block except, at most once each and in this order, SpecialFolder,
/// KnownFolder, Tracker and the known PropertyStore sets. In particular the
/// EnvironmentVariable, Console, Darwin, IconEnvironment, Shim and
/// VistaAndAboveIDList blocks are refused. The target itself is proven by the
/// exact native COM GetPath match, never by these bytes alone.
pub fn validate_link_bytes(bytes: &[u8]) -> io::Result<LinkLayout> {
    if bytes.len() < 80
        || bytes.len() > 65536
        || u32_at(bytes, 0)? != 76
        || bytes[4..20] != [1, 20, 2, 0, 0, 0, 0, 0, 192, 0, 0, 0, 0, 0, 0, 70]
    {
        return Err(invalid("unsupported shell link header"));
    }
    let flags = u32_at(bytes, 20)?;
    if flags & !(LINK_REQUIRED_FLAGS | LINK_ARGUMENTS_FLAG) != 0
        || flags & LINK_REQUIRED_FLAGS != LINK_REQUIRED_FLAGS
        || u32_at(bytes, 24)? & !0x20 != 0
        || u32_at(bytes, 56)? != 0
        || u32_at(bytes, 60)? != 1
        || bytes[64..76].iter().any(|b| *b != 0)
    {
        return Err(invalid("unsupported shortcut flags/show/hotkey"));
    }
    let id_list = validate_id_list(bytes, 76)?;
    let mut offset = 76 + 2 + id_list;
    let (info, local_base_path) = validate_link_info(bytes, offset)?;
    offset += info;
    let mut relative_path = String::new();
    for flag in [0x8, 0x10, 0x20] {
        if flags & flag != 0 {
            let count = u16_at(bytes, offset)?;
            let text = bytes
                .get(offset + 2..offset + 2 + count * 2)
                .ok_or_else(|| invalid("truncated shortcut string"))?;
            if count == 0 {
                return Err(invalid("empty shortcut string"));
            }
            if flag == 0x8 {
                let units: Vec<u16> = text
                    .chunks(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                relative_path = String::from_utf16(&units)
                    .map_err(|_| invalid("non Unicode shortcut relative path"))?;
            }
            offset += 2 + count * 2;
        }
    }
    let mut order = 0;
    loop {
        let size = u32_at(bytes, offset)? as usize;
        if size < 4 {
            if size != 0 || offset + 4 != bytes.len() {
                return Err(invalid("shortcut ExtraData framing is unsupported"));
            }
            break;
        }
        let block = bytes
            .get(
                offset
                    ..offset
                        .checked_add(size)
                        .ok_or_else(|| invalid("ExtraData overflow"))?,
            )
            .ok_or_else(|| invalid("truncated shortcut ExtraData"))?;
        let signature = u32_at(block, 4)?;
        let position = [
            SPECIAL_FOLDER_BLOCK,
            KNOWN_FOLDER_BLOCK,
            TRACKER_BLOCK,
            PROPERTY_STORE_BLOCK,
        ]
        .iter()
        .position(|s| *s == signature)
        .ok_or_else(|| invalid("shortcut ExtraData is unsupported"))?;
        // IDList offsets of folder blocks must point into the admitted list.
        let valid = position >= order
            && match signature {
                SPECIAL_FOLDER_BLOCK => size == 0x10 && (u32_at(block, 12)? as usize) < id_list,
                KNOWN_FOLDER_BLOCK => size == 0x1c && (u32_at(block, 24)? as usize) < id_list,
                TRACKER_BLOCK => {
                    size == 0x60
                        && u32_at(block, 8)? == 0x58
                        && u32_at(block, 12)? == 0
                        && block[16..32].is_ascii()
                }
                _ => (12..=1024).contains(&size) && validate_property_store(&block[8..]).is_ok(),
            };
        if !valid {
            return Err(invalid("shortcut ExtraData is unsupported"));
        }
        order = position + 1;
        offset += size;
    }
    Ok(LinkLayout {
        flags,
        local_base_path,
        relative_path,
    })
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
