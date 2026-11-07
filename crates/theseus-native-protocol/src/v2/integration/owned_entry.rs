//! Immutable owned-mode entry ceilings. Parsing actual installed files remains Host authority.
use super::{DesktopExecutable, NativeArtifactCommitment};
use crate::v2::{
    Error, LocalEndpoint,
    broker::{DesktopManifest, DesktopMode},
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OwnedDesktopEntryDefinition {
    MacosBundle {
        app_leaf: String,
        bundle_identifier: String,
        executable_leaf: String,
        info_plist: NativeArtifactCommitment,
    },
    WindowsShortcut,
}
impl OwnedDesktopEntryDefinition {
    pub fn os(&self) -> &'static str {
        match self {
            Self::MacosBundle { .. } => "macos",
            Self::WindowsShortcut => "windows",
        }
    }
    pub fn entry_relative_path(&self, desktop: &DesktopManifest) -> String {
        match self {
            Self::MacosBundle { app_leaf, .. } => format!("{app_leaf}/Contents/Info.plist"),
            Self::WindowsShortcut => desktop.entry.clone(),
        }
    }
    pub fn validate(
        &self,
        desktop: &DesktopManifest,
        executable: &DesktopExecutable,
        entry_length: u64,
    ) -> Result<(), Error> {
        if desktop.allowed_modes != [DesktopMode::OwnedExecutable]
            || desktop.namespace_policy != "exclusive_quiescent"
            || entry_length == 0
            || entry_length > 65536
            || crate::v2::Digest::parse(&desktop.entry_digest)?.as_bytes() == &[0; 32]
            || crate::v2::Digest::parse(&executable.sha256)?.as_bytes() == &[0; 32]
        {
            return Err(Error::InvalidValue);
        }
        match self {
            Self::MacosBundle {
                app_leaf,
                bundle_identifier,
                executable_leaf,
                info_plist,
            } => {
                info_plist.validate()?;
                if !leaf(app_leaf)
                    || !app_leaf.ends_with(".app")
                    || app_leaf.len() <= 4
                    || !leaf(executable_leaf)
                    || desktop.entry != *app_leaf
                    || executable.root != desktop.entry_root
                    || executable.relative_path
                        != format!("{app_leaf}/Contents/MacOS/{executable_leaf}")
                    || info_plist.length != entry_length
                    || info_plist.sha256 != desktop.entry_digest
                    || bundle_identifier.len() > 255
                    || !bundle_identifier.contains('.')
                    || !bundle_identifier.split('.').all(|p| {
                        !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    })
                    || !matches!(desktop.ready_endpoint, LocalEndpoint::UnixPath { .. })
                {
                    return Err(Error::InvalidValue);
                }
            }
            Self::WindowsShortcut => {
                let root = executable.root.as_bytes();
                if root.len() <= 3
                    || root.len() > 1024
                    || !root[0].is_ascii_alphabetic()
                    || root[1] != b':'
                    || root[2] != b'\\'
                    || !executable.root[3..].split('\\').all(windows_leaf)
                    || !windows_leaf(&desktop.entry)
                    || !desktop.entry.ends_with(".lnk")
                    || desktop.entry.len() <= 4
                    || entry_length < 76
                    || !windows_leaf(&executable.relative_path)
                    || !executable.relative_path.ends_with(".exe")
                    || executable.relative_path.len() <= 4
                    || executable.root != desktop.entry_root
                    || executable.working_directory != executable.root
                    || executable.arguments.first()
                        != Some(&format!(
                            "{}\\{}",
                            executable.root, executable.relative_path
                        ))
                    || !matches!(desktop.ready_endpoint, LocalEndpoint::NamedPipe { .. })
                {
                    return Err(Error::InvalidValue);
                }
            }
        }
        Ok(())
    }
    pub(super) fn same_namespace(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::MacosBundle {
                    app_leaf: a,
                    bundle_identifier: ai,
                    executable_leaf: ae,
                    ..
                },
                Self::MacosBundle {
                    app_leaf: b,
                    bundle_identifier: bi,
                    executable_leaf: be,
                    ..
                },
            ) => a == b && ai == bi && ae == be,
            (Self::WindowsShortcut, Self::WindowsShortcut) => true,
            _ => false,
        }
    }
}
fn leaf(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && !matches!(v, "." | "..")
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
// Closed declarative shape only. Actual IShellLink/native resource parsing is separate.
fn windows_leaf(v: &str) -> bool {
    if !leaf(v) || v.ends_with(['.', ' ']) {
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
