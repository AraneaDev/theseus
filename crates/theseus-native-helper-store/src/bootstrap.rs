use crate::StoreError;
use theseus_host::fs::HostDir;
use theseus_native_protocol::v2::{BindingId, Digest, Os, WorkspaceMarker, ledger::Record};
pub struct BootstrapRequest {
    pub binding: BindingId,
    pub manifest_digest: Digest,
    pub os: Os,
}
pub struct BootstrapReceipt {
    marker: WorkspaceMarker,
    header: Record,
}
impl BootstrapReceipt {
    pub fn workspace_marker(&self) -> WorkspaceMarker {
        self.marker
    }
    pub fn header(&self) -> &Record {
        &self.header
    }
}
pub(crate) const LEDGER: &str = "ledger.bin";
pub(crate) const MARKER: &str = ".theseus-workspace-marker";
pub fn bootstrap(
    ledger_root: &HostDir,
    workspace_root: &HostDir,
    request: BootstrapRequest,
) -> Result<BootstrapReceipt, StoreError> {
    bootstrap_with(
        ledger_root,
        workspace_root,
        request,
        None,
        theseus_host::random_bytes,
        |file, bytes| {
            use std::io::Write;
            file.write_all(bytes)?;
            file.sync_all()
        },
    )
}
pub fn bootstrap_native(
    ledger_root: &HostDir,
    workspace_root: &HostDir,
    request: BootstrapRequest,
    provider_epoch: Digest,
) -> Result<BootstrapReceipt, StoreError> {
    bootstrap_with(
        ledger_root,
        workspace_root,
        request,
        Some(provider_epoch),
        theseus_host::random_bytes,
        |f, b| {
            use std::io::Write;
            f.write_all(b)?;
            f.sync_all()
        },
    )
}
pub(crate) fn bootstrap_broker(
    ledger_root: &HostDir,
    workspace_root: &HostDir,
    header: &theseus_native_protocol::v2::broker::IntegrationBrokerHeader,
) -> Result<BootstrapReceipt, StoreError> {
    if header.linux_gio_qualification.is_some() {
        return Err(StoreError::BindingMismatch);
    }
    bootstrap_broker_profile(ledger_root, workspace_root, header)
}
pub(crate) fn bootstrap_broker_profile(
    ledger_root: &HostDir,
    workspace_root: &HostDir,
    header: &theseus_native_protocol::v2::broker::IntegrationBrokerHeader,
) -> Result<BootstrapReceipt, StoreError> {
    header.validate().map_err(|_| StoreError::BindingMismatch)?;
    let request = BootstrapRequest {
        binding: BindingId::parse(&header.binding).map_err(|_| StoreError::BindingMismatch)?,
        manifest_digest: Digest::parse(&header.manifest_digest)
            .map_err(|_| StoreError::BindingMismatch)?,
        os: match header.os.as_str() {
            "linux" => Os::Linux,
            "macos" => Os::Macos,
            "windows" => Os::Windows,
            _ => return Err(StoreError::BindingMismatch),
        },
    };
    bootstrap_profile_with(
        ledger_root,
        workspace_root,
        request,
        None,
        None,
        Some(header),
        theseus_host::random_bytes,
        |file, bytes| {
            use std::io::Write;
            file.write_all(bytes)?;
            file.sync_all()
        },
    )
}
pub fn bootstrap_integration(
    ledger_root: &HostDir,
    workspace_root: &HostDir,
    request: BootstrapRequest,
    context: &theseus_native_protocol::v2::IntegrationRuntimeContext,
    core_manifest_digest: Digest,
    integration_manifest_digest: Digest,
    artifact_commitment: Digest,
) -> Result<BootstrapReceipt, StoreError> {
    if context.linux_gio_qualification.is_some() {
        return Err(StoreError::BindingMismatch);
    }
    bootstrap_integration_profile(
        ledger_root,
        workspace_root,
        request,
        context,
        core_manifest_digest,
        integration_manifest_digest,
        artifact_commitment,
    )
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn bootstrap_integration_profile(
    ledger_root: &HostDir,
    workspace_root: &HostDir,
    request: BootstrapRequest,
    context: &theseus_native_protocol::v2::IntegrationRuntimeContext,
    core_manifest_digest: Digest,
    integration_manifest_digest: Digest,
    artifact_commitment: Digest,
) -> Result<BootstrapReceipt, StoreError> {
    use theseus_native_protocol::v2;
    context
        .validate_os(request.os)
        .map_err(|_| StoreError::BindingMismatch)?;
    if v2::integration_manifest_digest(core_manifest_digest, integration_manifest_digest)
        != request.manifest_digest
        || context.binding != request.binding.to_hex()
        || context.manifest_digest != request.manifest_digest.to_hex()
        || context.integration_manifest_digest != integration_manifest_digest.to_hex()
    {
        return Err(StoreError::BindingMismatch);
    }
    bootstrap_profile_with(
        ledger_root,
        workspace_root,
        request,
        None,
        Some((
            context,
            core_manifest_digest,
            integration_manifest_digest,
            artifact_commitment,
        )),
        None,
        theseus_host::random_bytes,
        |f, b| {
            use std::io::Write;
            f.write_all(b)?;
            f.sync_all()
        },
    )
}
fn bootstrap_with(
    ledger_root: &HostDir,
    workspace_root: &HostDir,
    request: BootstrapRequest,
    provider_epoch: Option<Digest>,
    random: impl FnOnce(&mut [u8]) -> std::io::Result<()>,
    write_sync: impl FnMut(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
) -> Result<BootstrapReceipt, StoreError> {
    bootstrap_profile_with(
        ledger_root,
        workspace_root,
        request,
        provider_epoch,
        None,
        None,
        random,
        write_sync,
    )
}
// Keep explicit authority inputs separate from the injected entropy and sync hooks.
#[allow(clippy::too_many_arguments)]
fn bootstrap_profile_with(
    ledger_root: &HostDir,
    workspace_root: &HostDir,
    request: BootstrapRequest,
    provider_epoch: Option<Digest>,
    integration: Option<(
        &theseus_native_protocol::v2::IntegrationRuntimeContext,
        Digest,
        Digest,
        Digest,
    )>,
    broker: Option<&theseus_native_protocol::v2::broker::IntegrationBrokerHeader>,
    random: impl FnOnce(&mut [u8]) -> std::io::Result<()>,
    mut write_sync: impl FnMut(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
) -> Result<BootstrapReceipt, StoreError> {
    use std::io;
    use theseus_host::fs::RelativePath;
    use theseus_native_protocol::v2::ledger::{Entry, encode};
    if ledger_root.same_object(workspace_root)? {
        return Err(StoreError::AliasedRoots);
    }
    // Probe both names before evaluating either result; no mutation on ambiguity.
    let marker_path = RelativePath::parse(MARKER)?;
    let ledger_path = RelativePath::parse(LEDGER)?;
    let marker_probe = workspace_root.open_file(&marker_path);
    let ledger_probe = ledger_root.open_file(&ledger_path);
    for probe in [marker_probe, ledger_probe] {
        match probe {
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Ok(_) => return Err(StoreError::BootstrapConflict),
            Err(error) => return Err(StoreError::Io(error)),
        }
    }
    let mut bytes = [0; 16];
    random(&mut bytes)?;
    let marker = if let Some(header) = broker {
        WorkspaceMarker::parse(&header.workspace_marker).map_err(|_| StoreError::BindingMismatch)?
    } else if let Some((context, ..)) = integration {
        WorkspaceMarker::parse(&context.workspace_marker)
            .map_err(|_| StoreError::BindingMismatch)?
    } else {
        WorkspaceMarker::from_bytes(bytes)
    };
    let header = Record::new(
        0,
        if let Some(header) = broker {
            Entry::IntegrationBrokerHeader {
                header: header.clone(),
            }
        } else if let Some((
            context,
            core_manifest_digest,
            integration_manifest_digest,
            artifact_commitment,
        )) = integration
        {
            Entry::IntegrationHeader {
                artifact_commitment,
                binding: request.binding,
                manifest_digest: request.manifest_digest,
                workspace_marker: marker,
                os: request.os,
                provider_epoch: Digest::parse(&context.provider_epoch)
                    .map_err(|_| StoreError::BindingMismatch)?,
                core_manifest_digest,
                integration_manifest_digest,
                context_digest: context.digest().map_err(|_| StoreError::BindingMismatch)?,
                ownership_id: context.ownership_id.clone(),
            }
        } else if let Some(provider_epoch) = provider_epoch {
            Entry::NativeHeader {
                binding: request.binding,
                manifest_digest: request.manifest_digest,
                workspace_marker: marker,
                os: request.os,
                capabilities: theseus_native_protocol::v2::NativeCapabilities,
                provider_epoch,
            }
        } else {
            Entry::Header {
                binding: request.binding,
                manifest_digest: request.manifest_digest,
                workspace_marker: marker,
                os: request.os,
            }
        },
    )?;
    let encoded = encode(&header)?;
    let mut marker_file = workspace_root.create_file(&marker_path)?;
    write_sync(&mut marker_file, marker.to_hex().as_bytes())?;
    let mut ledger_file = ledger_root.create_file(&ledger_path)?;
    write_sync(&mut ledger_file, &encoded)?;
    // Unix also makes the new directory entries durable; Windows files are
    // flushed (process-crash durability) by write_sync above.
    #[cfg(unix)]
    {
        workspace_root.sync_directory()?;
        ledger_root.sync_directory()?;
    }
    Ok(BootstrapReceipt { marker, header })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> BootstrapRequest {
        BootstrapRequest {
            binding: BindingId::from_bytes([1; 16]),
            manifest_digest: Digest::from_bytes([2; 32]),
            os: Os::Linux,
        }
    }
    #[test]
    fn rng_and_barrier_failures_preserve_partial_artifacts_without_receipts() {
        for stage in 0..3 {
            let tmp = tempfile::tempdir().unwrap();
            let l = HostDir::create_private(&tmp.path().join("l")).unwrap();
            let w = HostDir::create_private(&tmp.path().join("w")).unwrap();
            let mut calls = 0;
            let result = bootstrap_with(
                &l,
                &w,
                request(),
                None,
                |bytes| {
                    if stage == 0 {
                        Err(std::io::Error::from_raw_os_error(123))
                    } else {
                        bytes.fill(7);
                        Ok(())
                    }
                },
                |f, bytes| {
                    use std::io::Write;
                    f.write_all(bytes)?;
                    calls += 1;
                    if calls == stage {
                        Err(std::io::Error::from_raw_os_error(123))
                    } else {
                        f.sync_all()
                    }
                },
            );
            assert!(matches!(result,Err(StoreError::Io(ref e)) if e.raw_os_error()==Some(123)));
            assert_eq!(tmp.path().join("w").join(MARKER).exists(), stage > 0);
            assert_eq!(tmp.path().join("l").join(LEDGER).exists(), stage == 2);
            if stage > 0 {
                assert!(matches!(
                    bootstrap(&l, &w, request()),
                    Err(StoreError::BootstrapConflict)
                ));
            }
        }
    }
    #[test]
    fn marker_then_exclusive_ledger_race_preserves_both_artifacts() {
        let tmp = tempfile::tempdir().unwrap();
        let l = HostDir::create_private(&tmp.path().join("l")).unwrap();
        let w = HostDir::create_private(&tmp.path().join("w")).unwrap();
        let result = bootstrap_with(
            &l,
            &w,
            request(),
            None,
            |b| {
                b.fill(8);
                Ok(())
            },
            |f, bytes| {
                use std::io::Write;
                f.write_all(bytes)?;
                f.sync_all()?;
                std::fs::write(tmp.path().join("l").join(LEDGER), b"racer")
            },
        );
        assert!(
            matches!(result,Err(StoreError::Io(ref e)) if e.kind()==std::io::ErrorKind::AlreadyExists)
        );
        assert_eq!(
            std::fs::read(tmp.path().join("l").join(LEDGER)).unwrap(),
            b"racer"
        );
        assert_eq!(
            std::fs::read(tmp.path().join("w").join(MARKER)).unwrap(),
            b"08080808080808080808080808080808"
        );
        assert!(matches!(
            bootstrap(&l, &w, request()),
            Err(StoreError::BootstrapConflict)
        ));
    }
}
#[cfg(test)]
mod write_tests {
    use super::*;
    #[test]
    fn modeled_partial_marker_write_is_preserved_exactly() {
        let tmp = tempfile::tempdir().unwrap();
        let l = HostDir::create_private(&tmp.path().join("l")).unwrap();
        let w = HostDir::create_private(&tmp.path().join("w")).unwrap();
        let request = BootstrapRequest {
            binding: BindingId::from_bytes([1; 16]),
            manifest_digest: Digest::from_bytes([2; 32]),
            os: Os::Linux,
        };
        let result = bootstrap_with(
            &l,
            &w,
            request,
            None,
            |b| {
                b.fill(8);
                Ok(())
            },
            |f, _| {
                use std::io::Write;
                f.write_all(b"0")?;
                Err(std::io::Error::from_raw_os_error(123))
            },
        );
        assert!(result.is_err());
        assert_eq!(
            std::fs::read(tmp.path().join("w").join(MARKER)).unwrap(),
            b"0"
        );
    }
}

/// Bootstrap a declared desktop capability in the original integration ledger.
/// The header and complete immutable declaration are durable before this returns.
/// An interruption leaves an unacquired header; normal helper open never repairs
/// or invents the missing declaration from later peer input.
pub fn bootstrap_declared_integration(
    ledger_root: &HostDir,
    workspace_root: &HostDir,
    request: BootstrapRequest,
    context: &theseus_native_protocol::v2::IntegrationRuntimeContext,
    manifest: &theseus_native_protocol::v2::IntegrationManifest,
    artifact_commitment: Digest,
) -> Result<BootstrapReceipt, StoreError> {
    use theseus_native_protocol::v2::{self, ledger::Entry};
    manifest
        .validate_context(context, request.os)
        .map_err(|_| StoreError::BindingMismatch)?;
    if manifest.desktop.is_none() {
        return Err(StoreError::BindingMismatch);
    }
    let core =
        Digest::parse(&manifest.core_manifest_digest).map_err(|_| StoreError::BindingMismatch)?;
    let integration = manifest.digest().map_err(|_| StoreError::BindingMismatch)?;
    let binding = request.binding;
    let manifest_digest = request.manifest_digest;
    let os = request.os;
    let receipt = bootstrap_integration(
        ledger_root,
        workspace_root,
        request,
        context,
        core,
        integration,
        artifact_commitment,
    )?;
    let mut store = crate::LedgerStore::open_profile(
        ledger_root,
        workspace_root,
        crate::StoreBinding {
            binding,
            manifest_digest,
            workspace_marker: receipt.marker,
            os,
        },
        v2::HelperProfile::Integration {
            provider_epoch: Digest::parse(&context.provider_epoch)
                .map_err(|_| StoreError::BindingMismatch)?,
            core_manifest_digest: core,
            integration_manifest_digest: integration,
        },
    )?;
    store.append(Record::new(
        1,
        Entry::DesktopAuthorityDeclared {
            manifest: Box::new(manifest.clone()),
            context: Box::new(context.clone()),
        },
    )?)?;
    Ok(receipt)
}

/// Provider-only creation of a qualified helper ledger. All original files and
/// the trusted issuer capability are revalidated before any bootstrap effect.
#[allow(clippy::too_many_arguments)]
pub fn bootstrap_provider_declared_integration(
    ledger_root: &HostDir,
    workspace_root: &HostDir,
    request: BootstrapRequest,
    context: &theseus_native_protocol::v2::IntegrationRuntimeContext,
    manifest: &theseus_native_protocol::v2::IntegrationManifest,
    artifact_commitment: Digest,
    authority: &crate::ProviderLinuxGioQualification,
) -> Result<BootstrapReceipt, StoreError> {
    use theseus_native_protocol::v2::{self, ledger::Entry};
    authority.revalidate()?;
    authority
        .attestation()
        .validate_context(manifest, context)
        .map_err(|_| StoreError::BindingMismatch)?;
    let core =
        Digest::parse(&manifest.core_manifest_digest).map_err(|_| StoreError::BindingMismatch)?;
    let integration = manifest.digest().map_err(|_| StoreError::BindingMismatch)?;
    let binding = request.binding;
    let full = request.manifest_digest;
    let os = request.os;
    let receipt = bootstrap_integration_profile(
        ledger_root,
        workspace_root,
        request,
        context,
        core,
        integration,
        artifact_commitment,
    )?;
    let mut store = crate::LedgerStore::open_profile(
        ledger_root,
        workspace_root,
        crate::StoreBinding {
            binding,
            manifest_digest: full,
            workspace_marker: receipt.marker,
            os,
        },
        v2::HelperProfile::Integration {
            provider_epoch: Digest::parse(&context.provider_epoch)
                .map_err(|_| StoreError::BindingMismatch)?,
            core_manifest_digest: core,
            integration_manifest_digest: integration,
        },
    )?;
    store.append(Record::new(
        1,
        Entry::DesktopAuthorityDeclared {
            manifest: Box::new(manifest.clone()),
            context: Box::new(context.clone()),
        },
    )?)?;
    authority.revalidate()?;
    store.append_provider_qualification(Record::new(
        2,
        Entry::LinuxGioQualificationDeclared {
            attestation: Box::new(authority.attestation().clone()),
        },
    )?)?;
    Ok(receipt)
}
