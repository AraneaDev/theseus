//! Read-only CoreFoundation admission, shared by distinct owned/native launch paths.
use std::io;
use theseus_host::desktop::macos as host;
use theseus_native_protocol::v2;
pub(crate) fn admit(tuple: &v2::DesktopDefinition) -> io::Result<host::AdmittedBundle> {
    let (app_leaf, bundle_identifier, executable_leaf, info_plist) =
        match (&tuple.native, &tuple.owned_entry) {
            (
                Some(v2::NativeDesktopDefinition::MacosBundle {
                    app_leaf,
                    bundle_identifier,
                    executable_leaf,
                    info_plist,
                    ..
                }),
                None,
            )
            | (
                None,
                Some(v2::OwnedDesktopEntryDefinition::MacosBundle {
                    app_leaf,
                    bundle_identifier,
                    executable_leaf,
                    info_plist,
                }),
            ) => (app_leaf, bundle_identifier, executable_leaf, info_plist),
            _ => return Err(invalid("exact Mac bundle entry declaration missing")),
        };
    let x = &tuple.executable;
    let d = &tuple.desktop;
    let profile = host::BundleProfile {
        app: app_leaf.clone(),
        bundle_identifier: bundle_identifier.clone(),
        executable: executable_leaf.clone(),
        arguments: x.arguments[1..].to_vec(),
        info: host::ArtifactCommitment {
            length: info_plist.length,
            sha256: super::launch::digest(&info_plist.sha256)?,
        },
        executable_artifact: host::ArtifactCommitment {
            length: x.length,
            sha256: super::launch::digest(&x.sha256)?,
        },
    };
    host::AdmittedBundle::admit(host::RetainedBundle::open(
        std::path::Path::new(&d.entry_root),
        profile,
    )?)
}
fn invalid(s: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, s)
}
