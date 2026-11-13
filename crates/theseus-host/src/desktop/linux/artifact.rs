//! Retained original entry/executable artifacts and their bounded hashes.
use super::{
    AdmittedDefinition, ArtifactSlot, MAX_ARTIFACT, MAX_DEFINITION, absolute, invalid, parse_entry,
};
use crate::fs::{FileFingerprint, HostDir, RelativePath, RootPolicy, fingerprint, read_exact_at};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io,
    os::unix::{fs::PermissionsExt, io::AsRawFd},
    path::{Path, PathBuf},
};

pub(super) struct RetainedArtifact {
    pub(super) slot: ArtifactSlot,
    root: HostDir,
    relative: RelativePath,
    pub(super) file: File,
    fingerprint: FileFingerprint,
    executable: bool,
}
impl RetainedArtifact {
    pub(super) fn admit(slot: ArtifactSlot, max: u64, executable: bool) -> io::Result<Self> {
        if !slot.root.to_str().is_some_and(absolute)
            || slot.relative.len() > 2048
            || slot.length == 0
            || slot.length > max
            || slot.sha256 == [0; 32]
        {
            return Err(invalid("invalid admitted artifact slot"));
        }
        let relative = RelativePath::parse(&slot.relative)?;
        let root = HostDir::open(&slot.root, RootPolicy::AllComponentsNoFollow)?;
        let file = root.open_file(&relative)?;
        let fingerprint = fingerprint(&file)?;
        let retained = Self {
            slot,
            root,
            relative,
            file,
            fingerprint,
            executable,
        };
        retained.revalidate()?;
        Ok(retained)
    }
    pub(super) fn path(&self) -> PathBuf {
        self.slot.root.join(&self.slot.relative)
    }
    pub(super) fn revalidate(&self) -> io::Result<()> {
        let root = HostDir::open(&self.slot.root, RootPolicy::AllComponentsNoFollow)?;
        if !self.root.same_object(&root)? {
            return Err(invalid("original supplied artifact root replaced"));
        }
        let mapped = self.root.open_file(&self.relative)?;
        if fingerprint(&mapped)? != self.fingerprint || fingerprint(&self.file)? != self.fingerprint
        {
            return Err(invalid("original artifact file or mapping changed"));
        }
        if self.executable {
            let mode = self.file.metadata()?.permissions().mode();
            if mode & 0o111 == 0 || mode & 0o6000 != 0 {
                return Err(invalid("artifact is not a non-elevating executable"));
            }
            let size = unsafe {
                libc::fgetxattr(
                    self.file.as_raw_fd(),
                    c"security.capability".as_ptr(),
                    std::ptr::null_mut(),
                    0,
                )
            };
            if size > 0 {
                return Err(invalid("file capabilities are unsupported"));
            }
            if size < 0 {
                let error = io::Error::last_os_error();
                if !matches!(error.raw_os_error(), Some(libc::ENODATA | libc::ENOTSUP)) {
                    return Err(error);
                }
            }
        }
        if self.fingerprint.len != self.slot.length
            || hash_file(&self.file, self.slot.length)? != self.slot.sha256
        {
            return Err(invalid("artifact differs from supplied length/SHA256"));
        }
        if fingerprint(&self.file)? != self.fingerprint
            || fingerprint(&self.root.open_file(&self.relative)?)? != self.fingerprint
        {
            return Err(invalid("artifact changed during bounded hash"));
        }
        let root_after = HostDir::open(&self.slot.root, RootPolicy::AllComponentsNoFollow)?;
        if !self.root.same_object(&root_after)? {
            return Err(invalid("artifact root changed during read"));
        }
        Ok(())
    }
    fn bytes(&self) -> io::Result<Vec<u8>> {
        if self.slot.length > MAX_DEFINITION {
            return Err(invalid("definition exceeds bound"));
        }
        let mut bytes = vec![0; self.slot.length as usize];
        read_exact_at(&self.file, &mut bytes, 0)?;
        if hash_bytes(&bytes) != self.slot.sha256 {
            return Err(invalid("definition snapshot differs from supplied hash"));
        }
        self.revalidate()?;
        Ok(bytes)
    }
}
pub(super) fn hash_file(file: &File, length: u64) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut buffer = [0; 32_768];
    let mut offset = 0;
    while offset < length {
        let count = (length - offset).min(buffer.len() as u64) as usize;
        read_exact_at(file, &mut buffer[..count], offset)?;
        hash.update(&buffer[..count]);
        offset += count as u64;
    }
    Ok(hash.finalize().into())
}
pub(super) fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Original retained entry and executable. Neither can be rebuilt from receipt
/// bytes or a PID. Revalidation is sticky after any observed authority failure.
pub struct RetainedEntry {
    entry: RetainedArtifact,
    pub(super) executable: RetainedArtifact,
    pub(super) definition: AdmittedDefinition,
    pub(super) bytes: Vec<u8>,
    revoked: std::cell::Cell<bool>,
}
impl RetainedEntry {
    pub fn admit(definition: AdmittedDefinition) -> io::Result<Self> {
        let entry = RetainedArtifact::admit(definition.entry.clone(), MAX_DEFINITION, false)?;
        if !entry.path().extension().is_some_and(|v| v == "desktop") {
            return Err(invalid("entry slot must be a desktop file"));
        }
        let executable =
            RetainedArtifact::admit(definition.executable.clone(), MAX_ARTIFACT, true)?;
        if executable.path() != Path::new(&definition.command.executable) {
            return Err(invalid("Exec path differs from admitted executable slot"));
        }
        let bytes = entry.bytes()?;
        parse_entry(&bytes, &definition.command)?;
        Ok(Self {
            entry,
            executable,
            definition,
            bytes,
            revoked: std::cell::Cell::new(false),
        })
    }
    pub fn revalidate(&self) -> io::Result<()> {
        if self.revoked.get() {
            return Err(invalid("original desktop artifact authority revoked"));
        }
        let result = self
            .entry
            .revalidate()
            .and_then(|()| self.executable.revalidate());
        if result.is_err() {
            self.revoked.set(true);
        }
        result
    }
}
