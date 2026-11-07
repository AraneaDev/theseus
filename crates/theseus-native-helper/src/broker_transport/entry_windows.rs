//! Read-only original-apartment shortcut admission; no ShellExecute authority.
use std::io;
use theseus_host::desktop::windows as host;
use theseus_native_protocol::v2;
pub(crate) fn admit(
    tuple: &v2::DesktopDefinition,
    apartment: host::ComApartment,
) -> io::Result<host::AdmittedShortcut> {
    if !matches!(
        (&tuple.native, &tuple.owned_entry),
        (
            Some(v2::NativeDesktopDefinition::WindowsShortcut { .. }),
            None
        ) | (None, Some(v2::OwnedDesktopEntryDefinition::WindowsShortcut))
    ) {
        return Err(invalid("exact Windows shortcut entry declaration missing"));
    }
    let x = &tuple.executable;
    let d = &tuple.desktop;
    if x.root != d.entry_root || x.working_directory != x.root {
        return Err(invalid("closed shortcut root/cwd differs"));
    }

    let profile = host::ShortcutProfile {
        root: d.entry_root.clone(),
        entry: d.entry.clone(),
        executable: x.relative_path.clone(),
        arguments: x.arguments[1..].to_vec(),
        entry_artifact: host::ArtifactCommitment {
            length: tuple.entry_length,
            sha256: super::launch::digest(&d.entry_digest)?,
        },
        executable_artifact: host::ArtifactCommitment {
            length: x.length,
            sha256: super::launch::digest(&x.sha256)?,
        },
    };
    host::AdmittedShortcut::parse(apartment, host::RetainedShortcut::open(profile)?)
}
fn invalid(s: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, s)
}
