//! Original trusted-provider bootstrap authority, separate from serialized claims.
use crate::{LedgerStore, StoreError};
use sha2::{Digest as _, Sha256};
use std::{
    cell::Cell,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};
use theseus_host::fs::{
    FileFingerprint, HostDir, RelativePath, RootPolicy, fingerprint, read_exact_at,
};
use theseus_native_protocol::v2::{
    self, LinuxGioBuildAttestation, LinuxQualifiedArtifact, NativeLinuxAbi,
};
struct PinnedArtifact {
    slot: LinuxQualifiedArtifact,
    root: HostDir,
    relative: RelativePath,
    file: File,
    identity: FileFingerprint,
}
impl PinnedArtifact {
    fn open(slot: LinuxQualifiedArtifact) -> Result<Self, StoreError> {
        slot.validate().map_err(|_| StoreError::BindingMismatch)?;
        let root = HostDir::open(Path::new(&slot.root), RootPolicy::AllComponentsNoFollow)?;
        let relative = RelativePath::parse(&slot.relative_path)?;
        let file = root.open_file(&relative)?;
        let identity = fingerprint(&file)?;
        let pin = Self {
            slot,
            root,
            relative,
            file,
            identity,
        };
        pin.revalidate()?;
        Ok(pin)
    }
    fn revalidate(&self) -> Result<(), StoreError> {
        let root = HostDir::open(
            Path::new(&self.slot.root),
            RootPolicy::AllComponentsNoFollow,
        )?;
        if !self.root.same_object(&root)?
            || fingerprint(&self.file)? != self.identity
            || fingerprint(&self.root.open_file(&self.relative)?)? != self.identity
            || self.identity.len != self.slot.length
        {
            return Err(StoreError::BindingMismatch);
        }
        let mut header = [0u8; 64];
        read_exact_at(&self.file, &mut header, 0)?;
        let machine = match self.slot.native_abi {
            NativeLinuxAbi::LinuxX86_64 => 62u16,
            NativeLinuxAbi::LinuxAarch64 => 183u16,
        };
        if header[..7] != *b"\x7fELF\x02\x01\x01"
            || !matches!(u16::from_le_bytes([header[16], header[17]]), 2 | 3)
            || u16::from_le_bytes([header[18], header[19]]) != machine
            || header[20..24] != 1u32.to_le_bytes()
            || header[52..54] != 64u16.to_le_bytes()
        {
            return Err(StoreError::BindingMismatch);
        }
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 32768];
        let mut reader = &self.file;
        reader.seek(SeekFrom::Start(0))?;
        let mut left = self.slot.length;
        while left != 0 {
            let count = left.min(buffer.len() as u64) as usize;
            reader.read_exact(&mut buffer[..count])?;
            hash.update(&buffer[..count]);
            left -= count as u64;
        }
        if format!("{:x}", hash.finalize()) != self.slot.sha256
            || fingerprint(&self.file)? != self.identity
        {
            return Err(StoreError::BindingMismatch);
        }
        Ok(())
    }
}
struct RetainedGraph {
    pins: Vec<PinnedArtifact>,
    revoked: Cell<bool>,
}
impl RetainedGraph {
    fn open(attestation: &LinuxGioBuildAttestation) -> Result<Self, StoreError> {
        let mut pins = Vec::new();
        for artifact in attestation.artifacts() {
            pins.push(PinnedArtifact::open(artifact.clone())?);
        }
        Ok(Self {
            pins,
            revoked: Cell::new(false),
        })
    }
    fn revalidate(&self) -> Result<(), StoreError> {
        if self.revoked.get() {
            return Err(StoreError::BindingMismatch);
        }
        for pin in &self.pins {
            if let Err(error) = pin.revalidate() {
                self.revoked.set(true);
                return Err(error);
            }
        }
        Ok(())
    }
}
/// An explicit semantic attestation by the already trusted original Prepare
/// provider. Neither Deserialize nor a config/report hash constructs this type.
///
/// The caller MUST be the implementation of the operator-selected original
/// trusted provider Prepare, retaining its actual build outputs and qualified
/// SDK/image owners. It attests that these SAME immutable artifacts are valid
/// compiled host images and the declared SDK closure, not merely ELF-looking
/// bytes. The owning provider must establish/retain the protected bootstrap and
/// exclusive_quiescent SDK/build namespace through use. This safe Rust API is an
/// explicit trust-boundary act; file/hash/ABI observations cannot verify truthful
/// compiler/SDK provenance. Ordinary helper/config/request paths never call it.
/// Native SDK unsafe authority remains confined to the dedicated broker main.
pub struct ProviderLinuxGioQualification {
    attestation: LinuxGioBuildAttestation,
    graph: RetainedGraph,
}
impl ProviderLinuxGioQualification {
    pub fn attest_owned_build(attestation: LinuxGioBuildAttestation) -> Result<Self, StoreError> {
        attestation
            .validate()
            .map_err(|_| StoreError::BindingMismatch)?;
        let graph = RetainedGraph::open(&attestation)?;
        Ok(Self { attestation, graph })
    }
    pub fn attestation(&self) -> &LinuxGioBuildAttestation {
        &self.attestation
    }
    pub fn revalidate(&self) -> Result<(), StoreError> {
        self.graph.revalidate()
    }
}
/// Receiver proof minted only from an original private provider bootstrap ledger
/// and the actual retained graph. It is not cloneable, Debug, or serializable.
/// The caller keeps the same protected/quiescent provider namespace; this is not
/// confinement against a hostile same-UID or privileged writer.
pub struct VerifiedLinuxGioBootstrap {
    attestation: LinuxGioBuildAttestation,
    graph: RetainedGraph,
}
impl VerifiedLinuxGioBootstrap {
    pub fn attestation(&self) -> &LinuxGioBuildAttestation {
        &self.attestation
    }
    pub fn revalidate(&self) -> Result<(), StoreError> {
        self.graph.revalidate()
    }
}
impl LedgerStore {
    pub fn retain_linux_gio_qualification(
        &self,
    ) -> Result<Option<VerifiedLinuxGioBootstrap>, StoreError> {
        let Some(attestation) = v2::declared_linux_gio_qualification(self.records())? else {
            return Ok(None);
        };
        #[cfg(target_os = "linux")]
        let native = match std::env::consts::ARCH {
            "x86_64" => NativeLinuxAbi::LinuxX86_64,
            "aarch64" => NativeLinuxAbi::LinuxAarch64,
            _ => return Err(StoreError::BindingMismatch),
        };
        #[cfg(not(target_os = "linux"))]
        {
            let _ = attestation;
            Err(StoreError::BindingMismatch)
        }
        #[cfg(target_os = "linux")]
        {
            if attestation.sdk.native_abi != native {
                return Err(StoreError::BindingMismatch);
            }
            let graph = RetainedGraph::open(attestation)?;
            Ok(Some(VerifiedLinuxGioBootstrap {
                attestation: attestation.clone(),
                graph,
            }))
        }
    }
}
pub(crate) fn forbidden_qualification_append() -> StoreError {
    StoreError::Io(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "qualification records require original trusted-provider bootstrap",
    ))
}
