//! Precommitted desktop images. Keys select admitted tuples, never caller paths.
use super::{
    DesktopExecutable, IntegrationManifest, NativeDesktopDefinition, ReadinessDefinitionExpectation,
};
use crate::v2::{
    Digest, Error,
    broker::{DesktopManifest, DesktopMode},
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopDefinitionRole {
    Base,
    Configured,
    Target,
}
impl DesktopDefinitionRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Configured => "configured",
            Self::Target => "target",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopArtifactRole {
    Base,
    Target,
}
impl DesktopArtifactRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Target => "target",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopDefinitionKey {
    pub role: DesktopDefinitionRole,
    pub artifact: DesktopArtifactRole,
}
impl DesktopDefinitionKey {
    pub const CONFIGURED_TARGET: Self = Self {
        role: DesktopDefinitionRole::Configured,
        artifact: DesktopArtifactRole::Target,
    };
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopDefinition {
    pub key: DesktopDefinitionKey,
    pub entry_length: u64,
    pub desktop: DesktopManifest,
    pub executable: DesktopExecutable,
    pub readiness: ReadinessDefinitionExpectation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<NativeDesktopDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_lifetime: Option<super::OwnedDesktopLifetimeDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_entry: Option<super::OwnedDesktopEntryDefinition>,
}
impl DesktopDefinition {
    pub fn validate(&self) -> Result<(), Error> {
        self.desktop.validate()?;
        self.executable.validate(&self.desktop)?;
        self.readiness.validate()?;
        if self.entry_length == 0
            || self.entry_length > 65536
            || self.readiness.role != self.key.role.as_str()
            || self.readiness.artifact != self.key.artifact.as_str()
            || self.readiness.digest()?.to_hex() != self.desktop.expectation_digest
            || self.readiness.artifact_sha256 != self.executable.sha256
            || self.readiness.artifact_length != self.executable.length
        {
            return Err(Error::InvalidValue);
        }
        if let Some(owned) = self.owned_lifetime {
            owned.validate_mode(&self.desktop.allowed_modes)?;
            if self.native.is_some() {
                return Err(Error::InvalidValue);
            }
        }
        if let Some(entry) = &self.owned_entry {
            entry.validate(&self.desktop, &self.executable, self.entry_length)?;
            if self.native.is_some() || self.owned_lifetime.is_some() && entry.os() != "macos" {
                return Err(Error::InvalidValue);
            }
        }
        match &self.native {
            Some(native) => {
                native.validate(&self.desktop, &self.executable)?;
                if let NativeDesktopDefinition::MacosBundle { info_plist, .. } = native
                    && info_plist.length != self.entry_length
                {
                    return Err(Error::InvalidValue);
                }
            }
            None if self.desktop.allowed_modes != [DesktopMode::OwnedExecutable] => {
                return Err(Error::InvalidValue);
            }
            None => {}
        }
        Ok(())
    }
    pub fn entry_relative_path(&self) -> String {
        if let Some(native) = &self.native {
            native.entry_relative_path(&self.desktop)
        } else if let Some(entry) = &self.owned_entry {
            entry.entry_relative_path(&self.desktop)
        } else {
            self.desktop.entry.clone()
        }
    }
    pub fn digest(&self) -> Result<Digest, Error> {
        use sha2::{Digest as _, Sha256};
        self.validate()?;
        let mut hash = Sha256::new();
        hash.update(b"theseus.desktop.definition-v1\0");
        hash.update(serde_json::to_vec(self).map_err(|_| Error::InvalidValue)?);
        Ok(Digest::from_bytes(hash.finalize().into()))
    }
    pub fn same_namespace(&self, other: &Self) -> bool {
        let native_startup = match (&self.native, &other.native) {
            (
                Some(NativeDesktopDefinition::LinuxGio { sdk: a, .. }),
                Some(NativeDesktopDefinition::LinuxGio { sdk: b, .. }),
            ) => {
                a.glib_version == b.glib_version
                    && a.native_abi == b.native_abi
                    && a.helper_root == b.helper_root
                    && a.helper_relative_path == b.helper_relative_path
                    && a.helper == b.helper
                    && a.sdk_receipt_sha256 == b.sdk_receipt_sha256
                    && a.helper_receipt_sha256 == b.helper_receipt_sha256
                    && a.provenance == b.provenance
            }
            (
                Some(NativeDesktopDefinition::MacosBundle {
                    app_leaf: a,
                    bundle_identifier: ai,
                    executable_leaf: ae,
                    ..
                }),
                Some(NativeDesktopDefinition::MacosBundle {
                    app_leaf: b,
                    bundle_identifier: bi,
                    executable_leaf: be,
                    ..
                }),
            ) => a == b && ai == bi && ae == be,
            (
                Some(NativeDesktopDefinition::WindowsShortcut { .. }),
                Some(NativeDesktopDefinition::WindowsShortcut { .. }),
            )
            | (None, None) => true,
            _ => false,
        };
        native_startup
            && match (&self.owned_entry, &other.owned_entry) {
                (Some(a), Some(b)) => a.same_namespace(b),
                (None, None) => true,
                _ => false,
            }
            && self.owned_lifetime == other.owned_lifetime
            && (self.native.is_none()
                || self.executable.environment == other.executable.environment)
            && self.desktop.core_manifest_digest == other.desktop.core_manifest_digest
            && self.desktop.slot == other.desktop.slot
            && self.desktop.entry_root == other.desktop.entry_root
            && self.desktop.entry == other.desktop.entry
            && self.desktop.namespace_policy == other.desktop.namespace_policy
            && self.desktop.allowed_modes == other.desktop.allowed_modes
            && self.desktop.ready_endpoint == other.desktop.ready_endpoint
            && self.desktop.close_endpoint == other.desktop.close_endpoint
            && self.executable.root == other.executable.root
            && self.executable.relative_path == other.executable.relative_path
            && self.executable.working_directory == other.executable.working_directory
            && self.native.as_ref().map(NativeDesktopDefinition::os)
                == other.native.as_ref().map(NativeDesktopDefinition::os)
            && self.native.as_ref().map(NativeDesktopDefinition::lifetime)
                == other.native.as_ref().map(NativeDesktopDefinition::lifetime)
    }
}
impl IntegrationManifest {
    pub fn desktop_definition(
        &self,
        key: &DesktopDefinitionKey,
    ) -> Result<DesktopDefinition, Error> {
        if *key != DesktopDefinitionKey::CONFIGURED_TARGET {
            return self
                .desktop_definitions
                .iter()
                .find(|d| &d.key == key)
                .cloned()
                .ok_or(Error::InvalidValue);
        }
        let desktop = self.desktop.clone().ok_or(Error::InvalidValue)?;
        let path = self.native_entry_relative_path()?;
        let entry = self
            .artifact_slots
            .iter()
            .find(|a| {
                a.slot == desktop.slot
                    && a.relative_path == path
                    && a.sha256 == desktop.entry_digest
            })
            .ok_or(Error::InvalidValue)?;
        Ok(DesktopDefinition {
            key: *key,
            entry_length: entry.length,
            desktop,
            executable: self.desktop_executable.clone().ok_or(Error::InvalidValue)?,
            readiness: self.desktop_readiness.clone().ok_or(Error::InvalidValue)?,
            native: self.desktop_native.clone(),
            owned_lifetime: self.desktop_owned_lifetime,
            owned_entry: self.desktop_owned_entry.clone(),
        })
    }
    pub(super) fn validate_desktop_definitions(&self) -> Result<(), Error> {
        if self.desktop_definitions.is_empty() {
            return Ok(());
        }
        if self.desktop_definitions.len() > 5 {
            return Err(Error::LimitExceeded);
        }
        let original = self.desktop_definition(&DesktopDefinitionKey::CONFIGURED_TARGET)?;
        original.validate()?;
        let mut keys = std::collections::BTreeSet::from([DesktopDefinitionKey::CONFIGURED_TARGET]);
        let mut digests = std::collections::BTreeSet::from([original.digest()?.to_hex()]);
        for definition in &self.desktop_definitions {
            definition.validate()?;
            if !keys.insert(definition.key)
                || !digests.insert(definition.digest()?.to_hex())
                || !original.same_namespace(definition)
            {
                return Err(Error::InvalidValue);
            }
        }
        Ok(())
    }
}
