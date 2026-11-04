//! Bounded retained authority acquisition shared by shipping and fixture entry points.
//! Loading a saved report never calls this module.
use anyhow::{Result, ensure};
use sha2::{Digest as _, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use theseus_host::fs::{FileFingerprint, HostDir, RelativePath, RootPolicy, fingerprint};

pub struct RetainedLaunchFile {
    directory: HostDir,
    file: File,
    relative: RelativePath,
    parent: PathBuf,
    path: PathBuf,
    fingerprint: FileFingerprint,
    bytes: Vec<u8>,
    digest: String,
}
impl RetainedLaunchFile {
    pub fn open(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let directory = HostDir::open(parent, RootPolicy::AllComponentsNoFollow)?;
        let parent = parent.canonicalize()?;
        let leaf = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid launch filename"))?;
        let relative = RelativePath::parse(leaf)?;
        let mut file = directory.open_file(&relative)?;
        let original = fingerprint(&file)?;
        let mut bytes = Vec::new();
        (&mut file).take(65537).read_to_end(&mut bytes)?;
        ensure!(
            !bytes.is_empty() && bytes.len() <= 65536,
            "launch authority exceeds bound"
        );
        let source = Self {
            directory,
            file,
            relative,
            path: parent.join(leaf),
            parent,
            fingerprint: original,
            digest: format!("{:x}", Sha256::digest(&bytes)),
            bytes,
        };
        source.verify()?;
        Ok(source)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn sha256(&self) -> &str {
        &self.digest
    }
    pub fn verify(&self) -> Result<()> {
        let current = HostDir::open(&self.parent, RootPolicy::AllComponentsNoFollow)?;
        ensure!(
            self.directory.same_object(&current)?,
            "launch parent changed"
        );
        ensure!(
            self.fingerprint == fingerprint(&self.file)?
                && self.fingerprint == fingerprint(&self.directory.open_file(&self.relative)?)?,
            "launch source changed"
        );
        Ok(())
    }
    pub fn identity_matches(&self, trusted: &Self) -> Result<bool> {
        self.verify()?;
        trusted.verify()?;
        Ok(self.directory.same_object(&trusted.directory)?
            && self.fingerprint == trusted.fingerprint
            && self.digest == trusted.digest)
    }
}
pub trait TargetAuthorityLoader {
    fn load(&self, source: &RetainedLaunchFile) -> Result<crate::native::launch::LaunchAuthority>;
}
pub struct ShippingAuthorityLoader;
impl TargetAuthorityLoader for ShippingAuthorityLoader {
    fn load(&self, source: &RetainedLaunchFile) -> Result<crate::native::launch::LaunchAuthority> {
        crate::native::launch::LaunchAuthority::decode(source.bytes())
    }
}
pub(crate) fn load(
    path: &Path,
    loader: &dyn TargetAuthorityLoader,
) -> Result<crate::native::launch::LaunchAuthority> {
    let source = RetainedLaunchFile::open(path)?;
    let result = loader.load(&source);
    source.verify()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_source_rejects_replacement_and_byte_changes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("launch.json");
        std::fs::write(&path, b"trusted").unwrap();
        let trusted = RetainedLaunchFile::open(&path).unwrap();
        assert!(
            RetainedLaunchFile::open(&path)
                .unwrap()
                .identity_matches(&trusted)
                .unwrap()
        );
        std::fs::rename(&path, temp.path().join("original")).unwrap();
        std::fs::write(&path, b"trusted").unwrap();
        assert!(trusted.verify().is_err());
    }
    #[cfg(all(unix, feature = "native-integration-test-support"))]
    #[test]
    fn frozen_execution_authority_never_heals_after_mutability_is_observed() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("images");
        std::fs::create_dir(&directory).unwrap();
        let paths = [directory.join("helper"), directory.join("provider")];
        for path in &paths {
            std::fs::write(path, b"frozen").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
        let pins = FixtureExecutablePins::retain(
            paths
                .into_iter()
                .map(|path| (path, 6, format!("{:x}", Sha256::digest(b"frozen"))))
                .collect(),
        )
        .unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(pins.verify().is_err());
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(
            pins.verify().is_err(),
            "matching again cannot restore a revoked execution ceiling"
        );
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    #[test]
    fn shipping_decoder_never_accepts_program_authority() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("launch.json");
        std::fs::write(&path, br#"{"program":"/fixture","args":[]}"#).unwrap();
        assert!(load(&path, &ShippingAuthorityLoader).is_err());
        std::fs::write(&path, vec![b'x'; 65537]).unwrap();
        assert!(RetainedLaunchFile::open(&path).is_err());
    }
}

/// Test-controller artifact authority, held through deferred execution. Artifacts
/// live in a fixture-owned frozen directory, independent of Cargo's mutable output.
#[cfg(feature = "native-integration-test-support")]
pub struct FixtureExecutablePins {
    revoked: std::sync::atomic::AtomicBool,
    directory: HostDir,
    parent: PathBuf,
    pins: Vec<FixtureExecutablePin>,
    commitment: String,
}
#[cfg(feature = "native-integration-test-support")]
struct FixtureExecutablePin {
    path: PathBuf,
    leaf: RelativePath,
    file: File,
    identity: FileFingerprint,
    length: u64,
    sha256: String,
}
#[cfg(feature = "native-integration-test-support")]
impl FixtureExecutablePins {
    pub fn retain(pins: Vec<(PathBuf, u64, String)>) -> Result<Self> {
        ensure!(
            (2..=8).contains(&pins.len()),
            "bounded fixture executable set required"
        );
        let parent = pins[0]
            .0
            .parent()
            .ok_or_else(|| anyhow::anyhow!("fixture executable parent missing"))?
            .to_owned();
        let directory = HostDir::open(&parent, RootPolicy::AllComponentsNoFollow)?;
        let mut retained = Vec::new();
        let mut commitment = Sha256::new();
        commitment.update(b"theseus.fixture.executables.v1\0");
        for (path, length, sha256) in pins {
            ensure!(
                path.is_absolute()
                    && path.parent() == Some(parent.as_path())
                    && (1..=512 * 1024 * 1024).contains(&length),
                "fixture executables require one frozen directory"
            );
            ensure!(
                !retained
                    .iter()
                    .any(|p: &FixtureExecutablePin| p.path == path),
                "duplicate fixture executable"
            );
            let leaf = RelativePath::parse(
                path.file_name()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| anyhow::anyhow!("invalid executable leaf"))?,
            )?;
            let file = directory.open_file(&leaf)?;
            let identity = fingerprint(&file)?;
            commitment.update(path.to_string_lossy().as_bytes());
            commitment.update(length.to_be_bytes());
            commitment.update(sha256.as_bytes());
            retained.push(FixtureExecutablePin {
                path,
                leaf,
                file,
                identity,
                length,
                sha256,
            });
        }
        let pins = Self {
            revoked: std::sync::atomic::AtomicBool::new(false),
            directory,
            parent,
            pins: retained,
            commitment: format!("{:x}", commitment.finalize()),
        };
        pins.verify()?;
        Ok(pins)
    }
    pub fn commitment(&self) -> &str {
        &self.commitment
    }
    pub fn contains(&self, path: &Path) -> bool {
        self.pins.iter().any(|p| p.path == path)
    }
    pub fn verify(&self) -> Result<()> {
        use std::sync::atomic::Ordering;
        ensure!(
            !self.revoked.load(Ordering::Acquire),
            "original fixture executable authority revoked"
        );
        let result = self.verify_current();
        if result.is_err() {
            self.revoked.store(true, Ordering::Release);
        }
        result
    }
    fn verify_current(&self) -> Result<()> {
        ensure!(
            self.directory.same_object(&HostDir::open(
                &self.parent,
                RootPolicy::AllComponentsNoFollow
            )?)?,
            "fixture executable directory replaced"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                std::fs::metadata(&self.parent)?.permissions().mode() & 0o222 == 0,
                "fixture executable directory is no longer frozen"
            );
        }
        for pin in &self.pins {
            let mut named = self.directory.open_file(&pin.leaf)?;
            ensure!(
                fingerprint(&pin.file)? == pin.identity && fingerprint(&named)? == pin.identity,
                "fixture executable identity changed"
            );
            let metadata = named.metadata()?;
            ensure!(
                metadata.is_file()
                    && metadata.len() == pin.length
                    && metadata.permissions().readonly(),
                "fixture executable is mutable or has changed length"
            );
            let mut hash = Sha256::new();
            let mut buffer = [0; 65536];
            loop {
                let n = named.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hash.update(&buffer[..n]);
            }
            ensure!(
                format!("{:x}", hash.finalize()) == pin.sha256
                    && fingerprint(&named)? == pin.identity,
                "fixture executable bytes changed"
            );
        }
        Ok(())
    }
}
