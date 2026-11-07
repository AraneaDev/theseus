use super::{ArtifactCommitment, BundleProfile, invalid};
use crate::fs::{FileFingerprint, HostDir, LeafOpenResult, RootPolicy, fingerprint, read_exact_at};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::File,
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// Retains exact native root, parents and regular file owners. This is artifact
/// admission only; it cannot authorize a native launch without CF metadata.
#[derive(Debug)]
pub struct RetainedBundle {
    pub(super) root_path: PathBuf,
    pub(super) profile: BundleProfile,
    root: HostDir,
    app: HostDir,
    contents: HostDir,
    macos: HostDir,
    info: File,
    executable: File,
    info_identity: FileFingerprint,
    executable_identity: FileFingerprint,
}
fn open_leaf(root: &HostDir, leaf: &str) -> io::Result<File> {
    match root.open_leaf_classified(OsStr::new(leaf)) {
        LeafOpenResult::Opened(file) => Ok(file),
        LeafOpenResult::MissingSettled => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "bundle artifact absent",
        )),
        LeafOpenResult::FailedSettled(error) => Err(error),
        LeafOpenResult::Unsettled(owner) => Err(io::Error::other(owner)),
    }
}
fn hash(file: &File, expected: &ArtifactCommitment) -> io::Result<FileFingerprint> {
    let before = fingerprint(file)?;
    if before.len != expected.length {
        return Err(invalid("bundle artifact length mismatch"));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 16384];
    let mut position = 0;
    while position < before.len {
        let count = (before.len - position).min(buffer.len() as u64) as usize;
        read_exact_at(file, &mut buffer[..count], position)?;
        digest.update(&buffer[..count]);
        position += count as u64;
    }
    let after = fingerprint(file)?;
    let actual: [u8; 32] = digest.finalize().into();
    if before.canonical_bytes() != after.canonical_bytes() || actual != expected.sha256 {
        return Err(invalid("bundle artifact changed or SHA256 differs"));
    }
    Ok(after)
}
impl RetainedBundle {
    pub fn open(root_path: &Path, profile: BundleProfile) -> io::Result<Self> {
        profile.validate()?;
        let root_text = root_path
            .to_str()
            .ok_or_else(|| invalid("bundle URL requires UTF-8 path"))?;
        if !root_path.is_absolute()
            || root_text.len() > 1024
            || root_text.split('/').any(|p| matches!(p, "." | ".."))
        {
            return Err(invalid("bundle root must be an exact absolute path"));
        }
        let root = HostDir::open(root_path, RootPolicy::AllComponentsNoFollow)?;
        let app = root.open_child_name(OsStr::new(&profile.app))?;
        let contents = app.open_child_name(OsStr::new("Contents"))?;
        let macos = contents.open_child_name(OsStr::new("MacOS"))?;
        let info = open_leaf(&contents, "Info.plist")?;
        let executable = open_leaf(&macos, &profile.executable)?;
        if executable.metadata()?.permissions().mode() & 0o111 == 0 {
            return Err(invalid("bundle executable lacks executable permission"));
        }
        let info_identity = hash(&info, &profile.info)?;
        let executable_identity = hash(&executable, &profile.executable_artifact)?;
        let held = Self {
            root_path: root_path.into(),
            profile,
            root,
            app,
            contents,
            macos,
            info,
            executable,
            info_identity,
            executable_identity,
        };
        held.revalidate()?;
        Ok(held)
    }
    pub fn profile(&self) -> &BundleProfile {
        &self.profile
    }
    pub fn bundle_path(&self) -> PathBuf {
        self.root_path.join(&self.profile.app)
    }
    pub fn executable_path(&self) -> PathBuf {
        self.bundle_path()
            .join("Contents/MacOS")
            .join(&self.profile.executable)
    }
    pub fn info_bytes(&self) -> io::Result<Vec<u8>> {
        self.revalidate()?;
        let mut bytes = vec![0; self.profile.info.length as usize];
        read_exact_at(&self.info, &mut bytes, 0)?;
        self.revalidate()?;
        Ok(bytes)
    }
    pub fn revalidate(&self) -> io::Result<()> {
        let current = HostDir::open(&self.root_path, RootPolicy::AllComponentsNoFollow)?;
        if !self.root.same_directory_object(&current)? {
            return Err(invalid("original supplied bundle root replaced"));
        }
        let app = self.root.open_child_name(OsStr::new(&self.profile.app))?;
        let contents = app.open_child_name(OsStr::new("Contents"))?;
        let macos = contents.open_child_name(OsStr::new("MacOS"))?;
        if !self.app.same_directory_object(&app)?
            || !self.contents.same_directory_object(&contents)?
            || !self.macos.same_directory_object(&macos)?
        {
            return Err(invalid("original bundle parent replaced"));
        }
        for (file, original, expected, held) in [
            (
                open_leaf(&contents, "Info.plist")?,
                &self.info_identity,
                &self.profile.info,
                &self.info,
            ),
            (
                open_leaf(&macos, &self.profile.executable)?,
                &self.executable_identity,
                &self.profile.executable_artifact,
                &self.executable,
            ),
        ] {
            if fingerprint(&file)?.canonical_bytes() != original.canonical_bytes()
                || hash(held, expected)?.canonical_bytes() != original.canonical_bytes()
            {
                return Err(invalid("original bundle artifact replaced or changed"));
            }
        }
        Ok(())
    }
}
