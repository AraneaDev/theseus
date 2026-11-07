//! File/ledger models only: synthetic ELF-shaped data is never loaded or executed.
// Linux GIO qualification artifact roots are POSIX absolute paths by protocol
// contract (`LinuxQualifiedArtifact::validate`); a Windows temp root such as
// `C:\...` is correctly refused, so the file graph cannot be built there. Unix
// hosts keep both the Linux retention path and the non-Linux refusal branch.
#![cfg(unix)]
#[path = "../../theseus-native-protocol/tests/support/linux_gio_qualification.rs"]
mod support;
use sha2::{Digest as _, Sha256};
use theseus_host::fs::HostDir;
use theseus_native_helper_store::{
    self as store, BootstrapRequest, LedgerStore, ProviderLinuxGioQualification, StoreBinding,
};
use theseus_native_protocol::v2;
fn file_graph() -> (
    tempfile::TempDir,
    v2::IntegrationManifest,
    v2::IntegrationRuntimeContext,
    v2::LinuxGioBuildAttestation,
) {
    let temp = tempfile::tempdir().unwrap();
    let (mut m, mut c, mut a) = support::fixture();
    for (index, artifact) in a
        .sdk
        .images
        .iter_mut()
        .map(|i| &mut i.artifact)
        .chain(std::iter::once(&mut a.helper))
        .chain(a.applications.iter_mut().map(|app| &mut app.artifact))
        .enumerate()
    {
        let root = temp.path().join(format!("image{index}"));
        std::fs::create_dir(&root).unwrap();
        let mut bytes = vec![0; 128];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        bytes[127] = index as u8;
        std::fs::write(root.join(&artifact.relative_path), &bytes).unwrap();
        artifact.root = root.display().to_string();
        artifact.sha256 = format!("{:x}", Sha256::digest(bytes));
    }
    let app = &a.applications[0].artifact;
    m.desktop_executable.as_mut().unwrap().sha256 = app.sha256.clone();
    m.desktop.as_mut().unwrap().executable_digest = app.sha256.clone();
    m.desktop_readiness.as_mut().unwrap().artifact_sha256 = app.sha256.clone();
    m.desktop.as_mut().unwrap().expectation_digest = m
        .desktop_readiness
        .as_ref()
        .unwrap()
        .digest()
        .unwrap()
        .to_hex();
    let Some(v2::NativeDesktopDefinition::LinuxGio { sdk, .. }) = &mut m.desktop_native else {
        unreachable!()
    };
    sdk.helper_root = a.helper.root.clone();
    sdk.helper_relative_path = a.helper.relative_path.clone();
    sdk.helper.sha256 = a.helper.sha256.clone();
    sdk.sdk_receipt_sha256 = a.sdk.digest().unwrap().to_hex();
    sdk.helper_receipt_sha256 = a.helper.digest().unwrap().to_hex();
    sdk.application_receipt_sha256 = app.digest().unwrap().to_hex();
    let integration = m.digest().unwrap();
    let full = v2::integration_manifest_digest(
        v2::Digest::parse(&m.core_manifest_digest).unwrap(),
        integration,
    );
    c.linux_gio_qualification = None;
    c.integration_manifest_digest = integration.to_hex();
    c.manifest_digest = full.to_hex();
    let h = c.desktop_broker_header.as_mut().unwrap();
    h.linux_gio_qualification = None;
    h.manifest_digest = full.to_hex();
    a.integration_manifest_digest = integration.to_hex();
    a.manifest_digest = full.to_hex();
    a.base_context_digest = c.qualification_binding_digest().unwrap().to_hex();
    a.base_broker_header_digest = c
        .desktop_broker_header
        .as_ref()
        .unwrap()
        .qualification_binding_digest()
        .unwrap()
        .to_hex();
    a.applications[0].tuple_digest = m
        .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
        .unwrap()
        .digest()
        .unwrap()
        .to_hex();
    let reference = a.reference().unwrap();
    c.linux_gio_qualification = Some(reference.clone());
    c.desktop_broker_header
        .as_mut()
        .unwrap()
        .linux_gio_qualification = Some(reference);
    a.validate_context(&m, &c).unwrap();
    (temp, m, c, a)
}
fn request(c: &v2::IntegrationRuntimeContext) -> BootstrapRequest {
    BootstrapRequest {
        binding: v2::BindingId::parse(&c.binding).unwrap(),
        manifest_digest: v2::Digest::parse(&c.manifest_digest).unwrap(),
        os: v2::Os::Linux,
    }
}
fn profile(m: &v2::IntegrationManifest, c: &v2::IntegrationRuntimeContext) -> v2::HelperProfile {
    v2::HelperProfile::Integration {
        provider_epoch: v2::Digest::parse(&c.provider_epoch).unwrap(),
        core_manifest_digest: v2::Digest::parse(&m.core_manifest_digest).unwrap(),
        integration_manifest_digest: m.digest().unwrap(),
    }
}
#[test]
fn only_explicit_original_provider_attestation_bootstraps_qualification_before_lease() {
    let (temp, m, c, a) = file_graph();
    let ledger = HostDir::create_private(&temp.path().join("ledger")).unwrap();
    let workspace = HostDir::create_private(&temp.path().join("workspace")).unwrap();
    assert!(
        store::bootstrap_declared_integration(
            &ledger,
            &workspace,
            request(&c),
            &c,
            &m,
            v2::Digest::from_bytes([9; 32])
        )
        .is_err()
    );
    assert!(!temp.path().join("ledger/ledger.bin").exists());
    // This explicit trusted-issuer call models provider semantics. The synthetic
    // graph tests only retained-object checks, not truthful native SDK build origin.
    let issuer = ProviderLinuxGioQualification::attest_owned_build(a).unwrap();
    store::bootstrap_provider_declared_integration(
        &ledger,
        &workspace,
        request(&c),
        &c,
        &m,
        v2::Digest::from_bytes([9; 32]),
        &issuer,
    )
    .unwrap();
    let store = LedgerStore::open_profile(
        &ledger,
        &workspace,
        StoreBinding {
            binding: v2::BindingId::parse(&c.binding).unwrap(),
            manifest_digest: v2::Digest::parse(&c.manifest_digest).unwrap(),
            workspace_marker: v2::WorkspaceMarker::parse(&c.workspace_marker).unwrap(),
            os: v2::Os::Linux,
        },
        profile(&m, &c),
    )
    .unwrap();
    assert!(matches!(
        store.records()[2].entry(),
        v2::ledger::Entry::LinuxGioQualificationDeclared { .. }
    ));
    #[cfg(target_os = "linux")]
    {
        let retained = store.retain_linux_gio_qualification().unwrap().unwrap();
        retained.revalidate().unwrap();
        assert_eq!(
            retained.attestation().reference().unwrap(),
            c.linux_gio_qualification.unwrap()
        );
    }
    // A valid original Linux bootstrap never grants a non-Linux receiver
    // operational Linux authority. The issuer/bootstrap checks above still run.
    #[cfg(not(target_os = "linux"))]
    assert!(matches!(
        store.retain_linux_gio_qualification(),
        Err(store::StoreError::BindingMismatch)
    ));
}
#[test]
fn retained_original_graph_revokes_on_mapping_or_bytes_change_and_never_heals() {
    let (_temp, _m, _c, a) = file_graph();
    let path = std::path::Path::new(&a.helper.root).join(&a.helper.relative_path);
    let bytes = std::fs::read(&path).unwrap();
    let issuer = ProviderLinuxGioQualification::attest_owned_build(a).unwrap();
    issuer.revalidate().unwrap();
    std::fs::write(&path, b"changed").unwrap();
    assert!(issuer.revalidate().is_err());
    std::fs::write(&path, bytes).unwrap();
    assert!(issuer.revalidate().is_err());
}
