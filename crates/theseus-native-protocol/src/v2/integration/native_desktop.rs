//! Immutable native definitions are ceilings and diagnostic commitments, not SDK authority.
use super::{DesktopExecutable, IntegrationManifest};
use crate::v2::{Digest, Error, LocalEndpoint, broker::*};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeBootstrap {
    TheseusFixtureReadyBootstrapV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeArtifactCommitment {
    pub length: u64,
    pub sha256: String,
}
impl NativeArtifactCommitment {
    pub fn validate(&self) -> Result<(), Error> {
        if self.length == 0 || self.length > crate::v2::MAX_NATIVE_FILE_BYTES {
            return Err(Error::InvalidValue);
        }
        nonzero(&self.sha256)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeLinuxAbi {
    LinuxX86_64,
    LinuxAarch64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeQualificationProvenance {
    ProviderBuildAttested,
    OriginalNativeVersionRun,
}
/// Receipts must separately be authenticated by the owning provider/fixture bridge.
/// Deserializing or validating this record never constructs that capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxGioQualification {
    pub glib_version: [u32; 3],
    pub native_abi: NativeLinuxAbi,
    pub helper_root: String,
    pub helper_relative_path: String,
    pub helper: NativeArtifactCommitment,
    pub sdk_receipt_sha256: String,
    pub helper_receipt_sha256: String,
    pub application_receipt_sha256: String,
    pub provenance: NativeQualificationProvenance,
}
impl LinuxGioQualification {
    pub fn validate(&self) -> Result<(), Error> {
        self.helper.validate()?;
        for receipt in [
            &self.sdk_receipt_sha256,
            &self.helper_receipt_sha256,
            &self.application_receipt_sha256,
        ] {
            nonzero(receipt)?;
        }
        if self.glib_version[0] != 2
            || self.glib_version[1] < 80
            || !absolute(&self.helper_root)
            || self.helper_root.ends_with('/')
            || !relative(&self.helper_relative_path)
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeDesktopDefinition {
    LinuxGio {
        bootstrap: NativeBootstrap,
        lifetime: NativeLifetimeRequirement,
        sdk: LinuxGioQualification,
    },
    MacosBundle {
        bootstrap: NativeBootstrap,
        lifetime: NativeLifetimeRequirement,
        app_leaf: String,
        bundle_identifier: String,
        executable_leaf: String,
        info_plist: NativeArtifactCommitment,
    },
    WindowsShortcut {
        bootstrap: NativeBootstrap,
        lifetime: NativeLifetimeRequirement,
    },
}
impl NativeDesktopDefinition {
    pub fn lifetime(&self) -> NativeLifetimeRequirement {
        match self {
            Self::LinuxGio { lifetime, .. }
            | Self::MacosBundle { lifetime, .. }
            | Self::WindowsShortcut { lifetime, .. } => *lifetime,
        }
    }
    pub fn authority(&self) -> NativeLifetimeAuthority {
        match self {
            Self::LinuxGio { .. } => NativeLifetimeAuthority::OriginalChild,
            Self::MacosBundle { .. } => NativeLifetimeAuthority::ManagerInstance,
            Self::WindowsShortcut { .. } => NativeLifetimeAuthority::OriginalPrimaryHandle,
        }
    }
    pub fn os(&self) -> &'static str {
        match self {
            Self::LinuxGio { .. } => "linux",
            Self::MacosBundle { .. } => "macos",
            Self::WindowsShortcut { .. } => "windows",
        }
    }
    /// macOS app-directory presence is separate from these committed metadata bytes.
    pub fn entry_relative_path(&self, desktop: &DesktopManifest) -> String {
        match self {
            Self::MacosBundle { app_leaf, .. } => format!("{app_leaf}/Contents/Info.plist"),
            _ => desktop.entry.clone(),
        }
    }
    pub fn validate_state(&self, state: &NativeLifetimeState) -> Result<(), Error> {
        state.validate_platform(self.os())?;
        if state.authority != self.authority() || state.required != self.lifetime() {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn validate(
        &self,
        desktop: &DesktopManifest,
        executable: &DesktopExecutable,
    ) -> Result<(), Error> {
        if desktop.allowed_modes != [DesktopMode::NativeActivation]
            || desktop.namespace_policy != "exclusive_quiescent"
        {
            return Err(Error::InvalidValue);
        }
        let expected = match self {
            Self::MacosBundle { .. } => NativeLifetimeRequirement::ActivationInstanceTerminated,
            _ => NativeLifetimeRequirement::OwnedPrimaryExited,
        };
        if self.lifetime() != expected {
            return Err(Error::InvalidValue);
        }
        match self {
            Self::LinuxGio { sdk, .. } => {
                sdk.validate()?;
                if !desktop.entry.ends_with(".desktop")
                    || !matches!(desktop.ready_endpoint, LocalEndpoint::UnixPath { .. })
                {
                    return Err(Error::InvalidValue);
                }
            }
            Self::MacosBundle {
                app_leaf,
                bundle_identifier,
                executable_leaf,
                info_plist,
                ..
            } => {
                info_plist.validate()?;
                if crate::v2::LeafName::new(app_leaf).is_err()
                    || !app_leaf.ends_with(".app")
                    || desktop.entry != *app_leaf
                    || crate::v2::LeafName::new(executable_leaf).is_err()
                    || info_plist.length > 65536
                    || info_plist.sha256 != desktop.entry_digest
                    || executable.root != desktop.entry_root
                    || executable.relative_path
                        != format!("{app_leaf}/Contents/MacOS/{executable_leaf}")
                    || bundle_identifier.is_empty()
                    || bundle_identifier.len() > 255
                    || !bundle_identifier
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
                    || !matches!(desktop.ready_endpoint, LocalEndpoint::UnixPath { .. })
                {
                    return Err(Error::InvalidValue);
                }
            }
            Self::WindowsShortcut { .. } => {
                if !desktop.entry.ends_with(".lnk")
                    || !executable.relative_path.ends_with(".exe")
                    || !matches!(desktop.ready_endpoint, LocalEndpoint::NamedPipe { .. })
                {
                    return Err(Error::InvalidValue);
                }
            }
        }
        Ok(())
    }
}
impl IntegrationManifest {
    pub fn native_entry_relative_path(&self) -> Result<String, Error> {
        self.desktop_entry_relative_path()
    }
    pub fn desktop_entry_relative_path(&self) -> Result<String, Error> {
        let desktop = self.desktop.as_ref().ok_or(Error::InvalidValue)?;
        Ok(if let Some(native) = &self.desktop_native {
            native.entry_relative_path(desktop)
        } else if let Some(entry) = &self.desktop_owned_entry {
            entry.entry_relative_path(desktop)
        } else {
            desktop.entry.clone()
        })
    }
}
fn nonzero(value: &str) -> Result<(), Error> {
    if Digest::parse(value)?.as_bytes() == &[0; 32] {
        Err(Error::InvalidValue)
    } else {
        Ok(())
    }
}
fn absolute(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 1024
        && !value.contains('\0')
        && !value.split('/').any(|p| matches!(p, "." | ".."))
}
fn relative(value: &str) -> bool {
    value.len() <= 1024
        && value.split('/').count() <= 8
        && value
            .split('/')
            .all(|p| crate::v2::LeafName::new(p).is_ok())
}
