use serde::Deserialize;
use std::{fs::File, io, path::Path};
use theseus_host::fs::{HostDir, RelativePath, RootPolicy, fingerprint, read_exact_at};
use theseus_native_protocol::v2;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerServerConfig {
    #[cfg(feature = "test-support")]
    #[serde(default)]
    source_parent_eof: bool,
    kind: String,
    schema: u8,
    profile: String,
    pub(super) ledger_root: String,
    pub(super) workspace_root: String,
    pub(super) header: v2::broker::IntegrationBrokerHeader,
    pub(super) manifest: v2::IntegrationManifest,
}
impl BrokerServerConfig {
    fn candidate(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > 1_048_576 {
            return Err(invalid("broker config outside bound"));
        }
        let config: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        config.validate_declaration()?;
        Ok(config)
    }
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        let config = Self::candidate(bytes)?;
        config.validate()?;
        Ok(config)
    }
    fn read_config(path: &Path) -> io::Result<Vec<u8>> {
        if !path.is_absolute() {
            return Err(invalid("broker config path must be absolute"));
        }
        let parent = HostDir::open(
            path.parent()
                .ok_or_else(|| invalid("config parent missing"))?,
            RootPolicy::AllComponentsNoFollow,
        )?;
        let leaf = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| invalid("config leaf invalid"))?;
        let file = parent.open_file(&RelativePath::parse(leaf)?)?;
        bounded_file(&file)
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        Self::from_bytes(&Self::read_config(path)?)
    }
    #[cfg(target_os = "linux")]
    pub fn load_stock(path: &Path) -> io::Result<super::native_linux_factory::StockBrokerConfig> {
        use super::native_linux_factory::{QualifiedLinuxBrokerConfig, StockBrokerConfig};
        let config = Self::candidate(&Self::read_config(path)?)?;
        if config.manifest.desktop_native.is_none() {
            config.validate()?;
            return Ok(StockBrokerConfig::Standard(Box::new(config)));
        }
        if config.header.linux_gio_qualification.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "original Linux qualification bootstrap unavailable",
            ));
        }
        let ledger = HostDir::open(
            Path::new(&config.ledger_root),
            RootPolicy::AllComponentsNoFollow,
        )?;
        let workspace = HostDir::open(
            Path::new(&config.workspace_root),
            RootPolicy::AllComponentsNoFollow,
        )?;
        let store = theseus_native_helper_store::LedgerStore::open_broker(
            &ledger,
            &workspace,
            &config.header,
        )
        .map_err(io::Error::other)?;
        let proof = store
            .retain_linux_gio_qualification()
            .map_err(io::Error::other)?
            .ok_or_else(|| invalid("original provider-created Linux bootstrap absent"))?;
        config.validate_qualified(&proof)?;
        Ok(StockBrokerConfig::QualifiedLinux(Box::new(
            QualifiedLinuxBrokerConfig {
                config,
                store,
                proof,
            },
        )))
    }
    pub fn source_parent_eof(&self) -> bool {
        #[cfg(feature = "test-support")]
        {
            self.source_parent_eof
        }
        #[cfg(not(feature = "test-support"))]
        {
            false
        }
    }
    pub fn header(&self) -> &v2::broker::IntegrationBrokerHeader {
        &self.header
    }
    pub fn manifest(&self) -> &v2::IntegrationManifest {
        &self.manifest
    }
    pub(super) fn validate(&self) -> io::Result<()> {
        self.validate_declaration()?;
        #[cfg(target_os = "linux")]
        if self.manifest.desktop_native.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Linux native activation requires authenticated original SDK/application qualification bridge plus original verified bootstrap and injected factory",
            ));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    pub(super) fn validate_qualified(
        &self,
        proof: &theseus_native_helper_store::VerifiedLinuxGioBootstrap,
    ) -> io::Result<()> {
        self.validate_declaration()?;
        if self.manifest.desktop_native.is_none() {
            return Err(invalid("qualified Linux native definition absent"));
        }
        proof
            .attestation()
            .validate_manifest(&self.manifest)
            .map_err(io::Error::other)?;
        proof
            .attestation()
            .validate_broker_header(&self.header)
            .map_err(io::Error::other)?;
        proof.revalidate().map_err(io::Error::other)
    }
    fn validate_declaration(&self) -> io::Result<()> {
        if self.kind != "theseus_native_desktop_broker"
            || self.schema != 1
            || self.profile != "native_integration_broker_v1"
            || self.header.profile != self.profile
            || !absolute(&self.ledger_root)
            || !absolute(&self.workspace_root)
        {
            return Err(invalid("explicit broker config profile/roots required"));
        }
        self.header.validate().map_err(io::Error::other)?;
        // A combined manifest is the same immutable fullM; this broker
        // still dispatches desktop requests only and acquires no service rights.
        self.manifest.validate().map_err(io::Error::other)?;
        let core =
            v2::Digest::parse(&self.manifest.core_manifest_digest).map_err(io::Error::other)?;
        let full = v2::integration_manifest_digest(
            core,
            self.manifest.digest().map_err(io::Error::other)?,
        );
        if full.to_hex() != self.header.manifest_digest {
            return Err(invalid("broker complete desktop manifest/header mismatch"));
        }
        let desktop = self
            .manifest
            .desktop
            .as_ref()
            .ok_or_else(|| invalid("desktop declaration missing"))?;
        if desktop.allowed_modes != self.header.allowed_modes {
            return Err(invalid("desktop/header mode mismatch"));
        }
        match &self.manifest.desktop_native {
            Some(native) => {
                super::native_policy::initial_lifetime(native)?;
                if native.os() != self.header.os {
                    return Err(invalid("native definition/header platform differs"));
                }
                super::native_policy::validate_static_bootstrap(&self.manifest)?;
                if native.os() == "windows" {
                    let x = self
                        .manifest
                        .desktop_executable
                        .as_ref()
                        .ok_or_else(|| invalid("native executable absent"))?;
                    if x.root != desktop.entry_root || x.working_directory != x.root {
                        return Err(invalid(
                            "closed Windows shortcut requires exact common root/cwd",
                        ));
                    }
                }
                #[cfg(target_os = "linux")]
                if native.os() != "linux" {
                    return Err(invalid("native OS differs from original Linux broker"));
                }
                #[cfg(target_os = "macos")]
                if native.os() != "macos" {
                    return Err(invalid("native OS differs from broker"));
                }
                #[cfg(windows)]
                if native.os() != "windows" {
                    return Err(invalid("native OS differs from broker"));
                }
            }
            None if desktop.allowed_modes != [v2::broker::DesktopMode::OwnedExecutable] => {
                return Err(invalid("closed desktop mode required"));
            }
            None => {}
        }
        #[cfg(feature = "test-support")]
        if self.source_parent_eof
            && !matches!(
                self.header.display,
                v2::broker::SuppliedDisplay::SourceOwnedHeadless
            )
        {
            return Err(invalid(
                "source parent watch requires owned headless profile",
            ));
        }
        #[cfg(not(target_os = "linux"))]
        if self.manifest.desktop_native.is_none() && !super::launch::source_headless(self) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "interactive non-Linux entry adapter not integrated; owned headless source profile only",
            ));
        }
        for tuple in &self.manifest.desktop_definitions {
            if tuple.native.is_some() {
                super::native_policy::validate_bootstrap_tuple(tuple)?;
            }
        }
        self.entry_slot()?;
        Ok(())
    }
    pub(super) fn entry_slot(&self) -> io::Result<&v2::IntegrationArtifactSlot> {
        let desktop = self
            .manifest
            .desktop
            .as_ref()
            .ok_or_else(|| invalid("desktop declaration missing"))?;
        let relative = self
            .manifest
            .native_entry_relative_path()
            .map_err(io::Error::other)?;
        let mut slots = self.manifest.artifact_slots.iter().filter(|slot| {
            slot.slot == desktop.slot
                && slot.relative_path == relative
                && slot.sha256 == desktop.entry_digest
                && slot.length > 0
                && slot.length <= 65_536
        });
        let slot = slots
            .next()
            .ok_or_else(|| invalid("independently committed desktop entry length/slot missing"))?;
        if slots.next().is_some() {
            return Err(invalid("ambiguous desktop entry slot"));
        }
        Ok(slot)
    }
}
fn bounded_file(file: &File) -> io::Result<Vec<u8>> {
    let before = fingerprint(file)?;
    if before.len == 0 || before.len > 1_048_576 {
        return Err(invalid("broker config outside byte bound"));
    }
    let mut bytes = vec![0; before.len as usize];
    read_exact_at(file, &mut bytes, 0)?;
    if fingerprint(file)? != before {
        return Err(invalid("original broker config changed while reading"));
    }
    Ok(bytes)
}
fn absolute(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && Path::new(value).is_absolute()
        && !value.contains('\0')
        && !value.split(['/', '\\']).any(|v| matches!(v, "." | ".."))
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(all(test, target_os = "linux"))]
mod native_combined_admission_tests {
    use super::*;
    #[test]
    fn native_combined_declaration_is_valid_but_never_bypasses_original_linux_proof() {
        let mut raw = super::super::native_routing_tests::native_config();
        let service = super::super::generation_tests::combined_manifest_fixture();
        raw["manifest"]["service_authority"] =
            serde_json::to_value(service.service_authority).unwrap();
        raw["manifest"]["definitions"] = serde_json::to_value(service.definitions).unwrap();
        let manifest: v2::IntegrationManifest =
            serde_json::from_value(raw["manifest"].clone()).unwrap();
        manifest
            .validate()
            .expect("native combined declared manifest");
        raw["header"]["manifest_digest"] = serde_json::json!(
            v2::integration_manifest_digest(
                v2::Digest::parse(&manifest.core_manifest_digest).unwrap(),
                manifest.digest().unwrap()
            )
            .to_hex()
        );
        let bytes = serde_json::to_vec(&raw).unwrap();
        BrokerServerConfig::candidate(&bytes).expect("native combined static admission");
        let error = BrokerServerConfig::from_bytes(&bytes)
            .err()
            .expect("original Linux proof is still mandatory");
        assert!(
            error
                .to_string()
                .contains("authenticated original SDK/application qualification bridge")
        );
        raw["header"]["display"] = serde_json::json!({"kind":"source_owned_headless"});
        assert!(BrokerServerConfig::candidate(&serde_json::to_vec(&raw).unwrap()).is_err());
    }
}
