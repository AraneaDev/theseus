//! Closed provider build/SDK statements. Deserialization is evidence, never issuer authority.
use super::*;
use crate::v2::{self, broker::IntegrationBrokerHeader};
use serde::{Deserialize, Serialize};
fn nonzero(value: &str) -> Result<(), Error> {
    if Digest::parse(value)?.as_bytes() == &[0; 32] {
        Err(Error::InvalidValue)
    } else {
        Ok(())
    }
}
fn text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.contains('\0')
}
fn absolute(value: &str) -> bool {
    text(value, 1024)
        && value.starts_with('/')
        && !value.ends_with('/')
        && !value.split('/').any(|p| matches!(p, "." | ".."))
}
fn digest<T: Serialize>(domain: &[u8], value: &T) -> Result<Digest, Error> {
    use sha2::{Digest as _, Sha256};
    let bytes = serde_json::to_vec(value).map_err(|_| Error::InvalidValue)?;
    if bytes.len() > 32768 {
        return Err(Error::LimitExceeded);
    }
    let mut h = Sha256::new();
    h.update(domain);
    h.update(bytes);
    Ok(Digest::from_bytes(h.finalize().into()))
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxGioQualificationRef {
    pub schema: u8,
    pub attestation_digest: String,
    pub prepare_invocation_id: String,
    pub prepare_invocation_digest: String,
}
impl LinuxGioQualificationRef {
    pub fn validate(&self) -> Result<(), Error> {
        if self.schema != 1 || !text(&self.prepare_invocation_id, 128) {
            return Err(Error::InvalidValue);
        }
        nonzero(&self.attestation_digest)?;
        nonzero(&self.prepare_invocation_digest)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxQualifiedArtifact {
    pub root: String,
    pub relative_path: String,
    pub length: u64,
    pub sha256: String,
    pub native_abi: NativeLinuxAbi,
}
impl LinuxQualifiedArtifact {
    pub fn validate(&self) -> Result<(), Error> {
        if !absolute(&self.root)
            || self.relative_path.len() > 1024
            || self.relative_path.split('/').count() > 8
            || !self
                .relative_path
                .split('/')
                .all(|p| v2::LeafName::new(p).is_ok())
            || self.length < 64
            || self.length > v2::MAX_NATIVE_FILE_BYTES
        {
            return Err(Error::InvalidValue);
        }
        nonzero(&self.sha256)
    }
    /// Stable content/ABI commitment; source paths remain bound in the full attestation.
    /// This lets an owned build output qualify the identical installed image without
    /// changing immutable fullM for a provider's per-instance build directory.
    pub fn digest(&self) -> Result<Digest, Error> {
        self.validate()?;
        digest(
            b"theseus.linux.compiled-content-v1\0",
            &(self.length, &self.sha256, self.native_abi),
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxSdkImageRole {
    Gio,
    Glib,
    Gobject,
    Support,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxSdkImage {
    pub role: LinuxSdkImageRole,
    pub artifact: LinuxQualifiedArtifact,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxSdkGraph {
    pub glib_version: [u32; 3],
    pub native_abi: NativeLinuxAbi,
    pub qualification_profile: String,
    /// Exactly one Gio, Glib, and Gobject image, plus at most 32 explicit Support images.
    /// Existing attestation digest and encoded ledger record byte limits still apply.
    pub images: Vec<LinuxSdkImage>,
}
impl LinuxSdkGraph {
    pub fn validate(&self) -> Result<(), Error> {
        if self.glib_version[0] != 2
            || self.glib_version[1] < 80
            || self.qualification_profile != "gio_compiled_native_v1"
            || !(3..=35).contains(&self.images.len())
        {
            return Err(Error::InvalidValue);
        }
        let mut paths = std::collections::BTreeSet::new();
        for image in &self.images {
            image.artifact.validate()?;
            if image.artifact.native_abi != self.native_abi
                || !paths.insert((&image.artifact.root, &image.artifact.relative_path))
            {
                return Err(Error::InvalidValue);
            }
        }
        for required in [
            LinuxSdkImageRole::Gio,
            LinuxSdkImageRole::Glib,
            LinuxSdkImageRole::Gobject,
        ] {
            if self.images.iter().filter(|i| i.role == required).count() != 1 {
                return Err(Error::InvalidValue);
            }
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<Digest, Error> {
        self.validate()?;
        digest(b"theseus.linux.sdk-graph-v1\0", self)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxQualifiedApplication {
    pub definition: DesktopDefinitionKey,
    pub tuple_digest: String,
    pub artifact: LinuxQualifiedArtifact,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxGioBuildAttestation {
    pub schema: u8,
    pub provider: String,
    pub prepare_invocation_id: String,
    pub prepare_invocation_digest: String,
    pub launch_fingerprint: String,
    pub target_id: String,
    pub ownership_id: String,
    pub manifest_digest: String,
    pub core_manifest_digest: String,
    pub integration_manifest_digest: String,
    pub provider_epoch: String,
    pub binding: String,
    pub workspace_marker: String,
    pub base_context_digest: String,
    pub base_broker_header_digest: String,
    pub namespace_policy: String,
    pub sdk: LinuxSdkGraph,
    pub helper: LinuxQualifiedArtifact,
    pub applications: Vec<LinuxQualifiedApplication>,
}
impl LinuxGioBuildAttestation {
    pub fn validate(&self) -> Result<(), Error> {
        if self.schema != 1
            || !text(&self.provider, 256)
            || !text(&self.target_id, 128)
            || !text(&self.ownership_id, 128)
            || !text(&self.prepare_invocation_id, 128)
            || self.namespace_policy != "exclusive_quiescent"
            || !(1..=6).contains(&self.applications.len())
        {
            return Err(Error::InvalidValue);
        }
        v2::BindingId::parse(&self.binding)?;
        v2::WorkspaceMarker::parse(&self.workspace_marker)?;
        for value in [
            &self.prepare_invocation_digest,
            &self.launch_fingerprint,
            &self.manifest_digest,
            &self.core_manifest_digest,
            &self.integration_manifest_digest,
            &self.provider_epoch,
            &self.base_context_digest,
            &self.base_broker_header_digest,
        ] {
            nonzero(value)?;
        }
        if v2::integration_manifest_digest(
            Digest::parse(&self.core_manifest_digest)?,
            Digest::parse(&self.integration_manifest_digest)?,
        )
        .to_hex()
            != self.manifest_digest
        {
            return Err(Error::InvalidValue);
        }
        self.sdk.validate()?;
        self.helper.validate()?;
        if self.helper.native_abi != self.sdk.native_abi {
            return Err(Error::InvalidValue);
        }
        let mut keys = std::collections::BTreeSet::new();
        for app in &self.applications {
            app.artifact.validate()?;
            nonzero(&app.tuple_digest)?;
            if app.artifact.native_abi != self.sdk.native_abi || !keys.insert(app.definition) {
                return Err(Error::InvalidValue);
            }
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<Digest, Error> {
        self.validate()?;
        digest(b"theseus.linux.provider-build-attestation-v1\0", self)
    }
    pub fn reference(&self) -> Result<LinuxGioQualificationRef, Error> {
        Ok(LinuxGioQualificationRef {
            schema: 1,
            attestation_digest: self.digest()?.to_hex(),
            prepare_invocation_id: self.prepare_invocation_id.clone(),
            prepare_invocation_digest: self.prepare_invocation_digest.clone(),
        })
    }
    pub fn validate_broker_header(&self, header: &IntegrationBrokerHeader) -> Result<(), Error> {
        self.validate()?;
        header.validate()?;
        if header.os != "linux"
            || header.allowed_modes != [v2::broker::DesktopMode::NativeActivation]
            || header.linux_gio_qualification.as_ref() != Some(&self.reference()?)
            || header.qualification_binding_digest()?.to_hex() != self.base_broker_header_digest
            || header.target_id != self.target_id
            || header.ownership_id != self.ownership_id
            || header.manifest_digest != self.manifest_digest
            || header.provider_epoch != self.provider_epoch
            || header.binding != self.binding
            || header.workspace_marker != self.workspace_marker
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn validate_manifest(&self, manifest: &IntegrationManifest) -> Result<(), Error> {
        self.validate()?;
        manifest.validate()?;
        if manifest.core_manifest_digest != self.core_manifest_digest
            || manifest.digest()?.to_hex() != self.integration_manifest_digest
            || self.applications.len() != manifest.desktop_definitions.len() + 1
        {
            return Err(Error::InvalidValue);
        }
        for app in &self.applications {
            let tuple = manifest.desktop_definition(&app.definition)?;
            let Some(NativeDesktopDefinition::LinuxGio { sdk, .. }) = &tuple.native else {
                return Err(Error::InvalidValue);
            };
            if tuple.digest()?.to_hex() != app.tuple_digest
                || tuple.executable.length != app.artifact.length
                || tuple.executable.sha256 != app.artifact.sha256
                || sdk.glib_version != self.sdk.glib_version
                || sdk.native_abi != self.sdk.native_abi
                || sdk.helper_root != self.helper.root
                || sdk.helper_relative_path != self.helper.relative_path
                || sdk.helper.length != self.helper.length
                || sdk.helper.sha256 != self.helper.sha256
                || sdk.sdk_receipt_sha256 != self.sdk.digest()?.to_hex()
                || sdk.helper_receipt_sha256 != self.helper.digest()?.to_hex()
                || sdk.application_receipt_sha256 != app.artifact.digest()?.to_hex()
                || sdk.provenance != NativeQualificationProvenance::ProviderBuildAttested
            {
                return Err(Error::InvalidValue);
            }
        }
        Ok(())
    }
    pub fn validate_context(
        &self,
        manifest: &IntegrationManifest,
        context: &IntegrationRuntimeContext,
    ) -> Result<(), Error> {
        self.validate_manifest(manifest)?;
        manifest.validate_context(context, v2::Os::Linux)?;
        let header = context
            .desktop_broker_header
            .as_ref()
            .ok_or(Error::InvalidValue)?;
        self.validate_broker_header(header)?;
        if context.linux_gio_qualification.as_ref() != Some(&self.reference()?)
            || context.qualification_binding_digest()?.to_hex() != self.base_context_digest
            || context.binding != self.binding
            || context.ownership_id != self.ownership_id
            || context.provider_epoch != self.provider_epoch
            || context.workspace_marker != self.workspace_marker
            || context.manifest_digest != self.manifest_digest
        {
            return Err(Error::InvalidValue);
        }
        Ok(())
    }
    pub fn artifacts(&self) -> impl Iterator<Item = &LinuxQualifiedArtifact> {
        self.sdk
            .images
            .iter()
            .map(|image| &image.artifact)
            .chain(std::iter::once(&self.helper))
            .chain(self.applications.iter().map(|app| &app.artifact))
    }
}
impl IntegrationRuntimeContext {
    pub fn qualification_binding_digest(&self) -> Result<Digest, Error> {
        let mut base = self.clone();
        base.linux_gio_qualification = None;
        if let Some(header) = &mut base.desktop_broker_header {
            header.linux_gio_qualification = None;
        }
        base.digest()
    }
}
impl IntegrationBrokerHeader {
    pub fn qualification_binding_digest(&self) -> Result<Digest, Error> {
        let mut base = self.clone();
        base.linux_gio_qualification = None;
        base.digest()
    }
}
/// Validated bootstrap evidence only. Native qualification requires original
/// provider/store provenance plus retained actual graph owners at the receiver.
pub fn declared_linux_gio_qualification(
    records: &[v2::ledger::Record],
) -> Result<Option<&LinuxGioBuildAttestation>, v2::ledger::LedgerError> {
    use v2::ledger::{Entry, LedgerError};
    let declarations = records
        .iter()
        .filter_map(|record| match record.entry() {
            Entry::LinuxGioQualificationDeclared { attestation } => {
                Some((record, attestation.as_ref()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if declarations.is_empty() {
        return Ok(None);
    }
    if declarations.len() != 1 {
        return Err(LedgerError::InvalidTransition);
    }
    let (record, attestation) = declarations[0];
    match records.first().map(v2::ledger::Record::entry) {
        Some(Entry::IntegrationBrokerHeader { header })
            if record.index() == 1 && records.get(1) == Some(record) =>
        {
            attestation
                .validate_broker_header(header)
                .map_err(|_| LedgerError::InvalidTransition)?
        }
        Some(Entry::IntegrationHeader { context_digest, .. })
            if record.index() == 2 && records.get(2) == Some(record) =>
        {
            let Some(Entry::DesktopAuthorityDeclared { manifest, context }) =
                records.get(1).map(v2::ledger::Record::entry)
            else {
                return Err(LedgerError::InvalidTransition);
            };
            if context
                .digest()
                .map_err(|_| LedgerError::InvalidTransition)?
                != *context_digest
            {
                return Err(LedgerError::InvalidTransition);
            }
            attestation
                .validate_context(manifest, context)
                .map_err(|_| LedgerError::InvalidTransition)?;
        }
        _ => return Err(LedgerError::InvalidTransition),
    }
    Ok(Some(attestation))
}
