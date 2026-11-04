//! Original provider-bootstrap capability to one retained stock GIO factory.
use super::BrokerServerConfig;
use std::io;
use theseus_host::desktop::{linux as host, session::BoundSession};
use theseus_native_helper_store::{LedgerStore, VerifiedLinuxGioBootstrap};
use theseus_native_protocol::v2;
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
pub type LinuxFactoryInitializer = fn(
    &VerifiedLinuxGioBootstrap,
    &BrokerServerConfig,
    BoundSession,
) -> io::Result<host::DedicatedGioBroker>;
/// No decoder/Clone/public fields: only original private store retention constructs it.
pub struct QualifiedLinuxBrokerConfig {
    pub(super) config: BrokerServerConfig,
    pub(super) store: LedgerStore,
    pub(super) proof: VerifiedLinuxGioBootstrap,
}
pub enum StockBrokerConfig {
    Standard(Box<BrokerServerConfig>),
    QualifiedLinux(Box<QualifiedLinuxBrokerConfig>),
}
impl StockBrokerConfig {
    pub fn source_parent_eof(&self) -> bool {
        match self {
            Self::Standard(c) => c.source_parent_eof(),
            Self::QualifiedLinux(c) => c.config.source_parent_eof(),
        }
    }
}
pub(super) struct LinuxNativeFactory {
    proof: VerifiedLinuxGioBootstrap,
    original: Option<host::DedicatedGioBroker>,
    session: theseus_host::desktop::session::SessionBinding,
    finalize: super::native_linux::OriginalFinalizer,
}
impl LinuxNativeFactory {
    pub(super) fn new(
        proof: VerifiedLinuxGioBootstrap,
        original: host::DedicatedGioBroker,
        session: theseus_host::desktop::session::SessionBinding,
        finalize: super::native_linux::OriginalFinalizer,
    ) -> io::Result<Self> {
        let mut factory = Self {
            proof,
            original: None,
            session,
            finalize,
        };
        factory.restore(original)?;
        Ok(factory)
    }
    pub(super) fn revalidate(&self) -> io::Result<()> {
        self.proof.revalidate().map_err(io::Error::other)
    }
    pub(super) fn restore(&mut self, original: host::DedicatedGioBroker) -> io::Result<()> {
        self.revalidate()?;
        if self.original.is_some() {
            return Err(invalid("original native factory token collision"));
        }
        let a = self.proof.attestation();
        let sdk = original.sdk_qualification();
        if original.session_binding() != &self.session
            || !original.matches_qualified_sdk_images(&qualified_linux_sdk_images(&self.proof)?)
            || original.helper_admission().qualification.provenance
                != (host::QualificationProvenance::ProviderBuildAttested {
                    verified_receipt_digest: *a
                        .helper
                        .digest()
                        .map_err(io::Error::other)?
                        .as_bytes(),
                })
            || sdk.qualification_digest != *a.sdk.digest().map_err(io::Error::other)?.as_bytes()
            || sdk.glib_version != a.sdk.glib_version
            || sdk.helper_length != a.helper.length
            || sdk.helper_sha256 != super::launch::digest(&a.helper.sha256)?
        {
            return Err(invalid(
                "original Host token differs from independently retained SDK graph",
            ));
        }
        self.original = Some(original);
        Ok(())
    }
    // Keep exact original owner/entry and both channel rights explicit.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare(
        &mut self,
        config: &BrokerServerConfig,
        tuple: &v2::DesktopDefinition,
        session: BoundSession,
        binding: theseus_host::desktop::owned::ExecutableBinding,
        entry: host::RetainedEntry,
        ready: theseus_host::services::readiness::LocalChallenge,
        close: theseus_host::services::readiness::LocalChallenge,
    ) -> io::Result<super::native_linux::Prepared> {
        self.revalidate()?;
        self.proof
            .attestation()
            .validate_manifest(&config.manifest)
            .map_err(io::Error::other)?;
        let sdk_digest = *self
            .proof
            .attestation()
            .sdk
            .digest()
            .map_err(io::Error::other)?
            .as_bytes();
        let qualification =
            application_qualification(tuple, &self.proof.attestation().applications, sdk_digest)?;
        let original = self
            .original
            .take()
            .ok_or_else(|| invalid("original native factory token unavailable"))?;
        super::native_linux::Prepared::qualified(
            original,
            entry,
            super::launch::digest(&config.header.manifest_digest)?,
            qualification,
            session,
            binding,
            tuple
                .native
                .as_ref()
                .ok_or_else(|| invalid("native definition absent"))?,
            ready,
            close,
            self.finalize,
        )
    }
}
/// Data projection from the retained original proof; it does not admit an SDK.
pub fn qualified_linux_sdk_images(
    proof: &VerifiedLinuxGioBootstrap,
) -> io::Result<host::QualifiedSdkImages> {
    host::QualifiedSdkImages::new(
        proof
            .attestation()
            .sdk
            .images
            .iter()
            .map(|image| {
                Ok(host::QualifiedSdkImage {
                    role: match image.role {
                        v2::LinuxSdkImageRole::Gio => host::SdkImageRole::Gio,
                        v2::LinuxSdkImageRole::Glib => host::SdkImageRole::Glib,
                        v2::LinuxSdkImageRole::Gobject => host::SdkImageRole::Gobject,
                        v2::LinuxSdkImageRole::Support => host::SdkImageRole::Support,
                    },
                    artifact: host::ArtifactSlot {
                        root: image.artifact.root.clone().into(),
                        relative: image.artifact.relative_path.clone(),
                        length: image.artifact.length,
                        sha256: super::launch::digest(&image.artifact.sha256)?,
                    },
                })
            })
            .collect::<io::Result<Vec<_>>>()?,
    )
}
fn application_qualification(
    tuple: &v2::DesktopDefinition,
    apps: &[v2::LinuxQualifiedApplication],
    sdk_digest: [u8; 32],
) -> io::Result<host::CompiledArtifactQualification> {
    let mut found = apps.iter().filter(|a| a.definition == tuple.key);
    let a = found.next().ok_or_else(|| {
        invalid("selected compiled source application not independently qualified")
    })?;
    let Some(v2::NativeDesktopDefinition::LinuxGio { sdk, .. }) = &tuple.native else {
        return Err(invalid("selected native platform differs"));
    };
    a.artifact.validate().map_err(io::Error::other)?;
    if found.next().is_some()
        || a.tuple_digest != tuple.digest().map_err(io::Error::other)?.to_hex()
        || a.artifact.length != tuple.executable.length
        || a.artifact.sha256 != tuple.executable.sha256
        || a.artifact.native_abi != sdk.native_abi
        || sdk_digest == [0; 32]
    {
        return Err(invalid(
            "compiled source qualification differs from selected installed tuple",
        ));
    }
    Ok(host::CompiledArtifactQualification {
        length: a.artifact.length,
        sha256: super::launch::digest(&a.artifact.sha256)?,
        native_abi: match a.artifact.native_abi {
            v2::NativeLinuxAbi::LinuxX86_64 => host::NativeElfAbi::LinuxX86_64,
            v2::NativeLinuxAbi::LinuxAarch64 => host::NativeElfAbi::LinuxAarch64,
        },
        sdk_qualification_digest: sdk_digest,
        provenance: host::QualificationProvenance::ProviderBuildAttested {
            verified_receipt_digest: *a.artifact.digest().map_err(io::Error::other)?.as_bytes(),
        },
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use theseus_native_protocol::v2;
    #[test]
    fn native_startup_cwd_ceiling_refuses_mismatch_without_claiming_application_cwd() {
        use std::path::Path;
        assert!(
            super::super::launch::check_native_startup_cwd(
                Path::new("/owned-startup"),
                Path::new("/owned-startup")
            )
            .is_ok()
        );
        assert!(
            super::super::launch::check_native_startup_cwd(
                Path::new("/owned-startup"),
                Path::new("/other")
            )
            .is_err()
        );
    }
    #[test]
    fn config_qualification_reference_cannot_create_original_bootstrap_or_factory() {
        let temp = tempfile::tempdir().unwrap();
        let mut value = super::super::native_routing_tests::native_config();
        let ledger = temp.path().join("absent-ledger");
        let workspace = temp.path().join("absent-workspace");
        value["ledger_root"] = serde_json::json!(ledger);
        value["workspace_root"] = serde_json::json!(workspace);
        value["header"]["linux_gio_qualification"] = serde_json::json!({"schema":1,"attestation_digest":"aa".repeat(32),"prepare_invocation_id":"original","prepare_invocation_digest":"bb".repeat(32)});
        let bytes = serde_json::to_vec(&value).unwrap();
        let path = temp.path().join("broker.json");
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            BrokerServerConfig::from_bytes(&bytes).err().unwrap().kind(),
            std::io::ErrorKind::Unsupported
        );
        assert_eq!(
            BrokerServerConfig::load_stock(&path).err().unwrap().kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(!ledger.exists());
        assert!(!workspace.exists());
    }
    #[test]
    fn source_application_qualification_matches_installed_content_without_importing_source_path() {
        let mut value = super::super::native_routing_tests::native_config();
        value["manifest"]["desktop_executable"]["length"] = serde_json::json!(128);
        value["manifest"]["desktop_readiness"]["artifact_length"] = serde_json::json!(128);
        let e: v2::ReadinessDefinitionExpectation =
            serde_json::from_value(value["manifest"]["desktop_readiness"].clone()).unwrap();
        value["manifest"]["desktop"]["expectation_digest"] =
            serde_json::json!(e.digest().unwrap().to_hex());
        let manifest: v2::IntegrationManifest =
            serde_json::from_value(value["manifest"].clone()).unwrap();
        let tuple = manifest
            .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
            .unwrap();
        let app = v2::LinuxQualifiedApplication {
            definition: tuple.key,
            tuple_digest: tuple.digest().unwrap().to_hex(),
            artifact: v2::LinuxQualifiedArtifact {
                root: "/owned-build-output".into(),
                relative_path: "source-app".into(),
                length: tuple.executable.length,
                sha256: tuple.executable.sha256.clone(),
                native_abi: v2::NativeLinuxAbi::LinuxX86_64,
            },
        };
        let q = application_qualification(&tuple, std::slice::from_ref(&app), [9; 32]).unwrap();
        assert_eq!(q.length, tuple.executable.length);
        assert_eq!(
            q.sha256,
            super::super::launch::digest(&tuple.executable.sha256).unwrap()
        );
        let mut wrong = app.clone();
        wrong.tuple_digest = "99".repeat(32);
        assert!(application_qualification(&tuple, &[wrong], [9; 32]).is_err());
        let mut wrong = app;
        wrong.artifact.sha256 = "88".repeat(32);
        assert!(application_qualification(&tuple, &[wrong], [9; 32]).is_err());
    }
}
