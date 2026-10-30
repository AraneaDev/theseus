//! Exact source drafts and explicit adjacent Save Copy persistence.
use crate::{comparison, recipe};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

pub const MAX_DRAFT_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    Recipe,
    Suite,
}

#[derive(Debug, Clone)]
pub struct PreviewSource {
    pub path: PathBuf,
    pub digest: String,
    pub kind: DocumentKind,
}

#[derive(Debug, Clone)]
pub enum ValidatedDocument {
    Recipe(recipe::ValidatedRecipe),
    Suite(comparison::ValidatedSuite),
}

#[derive(Debug, Clone)]
pub struct DraftDocument {
    pub source: PreviewSource,
    pub text: String,
}

impl DraftDocument {
    /// Read the accepted source lazily and reject a changed or uneditable source.
    pub fn open(source: PreviewSource) -> Result<Self> {
        ensure!(
            source.path.is_absolute(),
            "preview source path must be absolute"
        );
        ensure!(
            source
                .path
                .canonicalize()
                .context("resolve preview source")?
                == source.path,
            "preview source path has changed"
        );
        let mut bytes = Vec::new();
        File::open(&source.path)
            .with_context(|| format!("open preview source {}", source.path.display()))?
            .take((MAX_DRAFT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .with_context(|| format!("read preview source {}", source.path.display()))?;
        ensure!(
            bytes.len() <= MAX_DRAFT_BYTES,
            "preview source exceeds 1 MiB: {}",
            source.path.display()
        );
        let text = String::from_utf8(bytes).context("preview source is not UTF-8")?;
        ensure!(
            format!("{:x}", Sha256::digest(text.as_bytes())) == source.digest,
            "preview source changed since validation: {}",
            source.path.display()
        );
        Ok(Self { source, text })
    }

    /// Revalidate the current raw draft against its original source directory.
    pub fn validate(&self) -> Result<ValidatedDocument> {
        ensure!(self.text.len() <= MAX_DRAFT_BYTES, "draft exceeds 1 MiB");
        validate_at(self.source.kind, &self.source.path, self.text.as_bytes())
    }

    /// Persist exact bytes as a new adjacent source and return its reloaded document.
    pub fn save_copy(&self, basename: &str) -> Result<ValidatedDocument> {
        self.validate().context("validate draft before Save Copy")?;
        ensure!(
            (1..=128).contains(&basename.len())
                && basename.ends_with(".toml")
                && basename
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')),
            "Save Copy name must be 1..128 ASCII [A-Za-z0-9._-] bytes ending .toml"
        );
        ensure!(
            self.source
                .path
                .file_name()
                .is_some_and(|name| name != basename),
            "Save Copy name must differ from the original basename"
        );
        let parent = self
            .source
            .path
            .parent()
            .context("preview source has no parent")?;
        ensure!(
            parent
                .canonicalize()
                .context("resolve preview source directory")?
                == parent,
            "preview source directory is no longer canonical"
        );
        let destination = parent.join(basename);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&destination)
            .with_context(|| format!("create Save Copy {}", destination.display()))?;
        // Once exclusive creation succeeds, every failure names the remaining copy.
        (|| -> Result<ValidatedDocument> {
            file.write_all(self.text.as_bytes())
                .context("write Save Copy")?;
            file.sync_all().context("sync Save Copy")?;
            drop(file);
            if let Err(error) = File::open(parent)
                .context("open Save Copy directory")?
                .sync_all()
                && !matches!(
                    error.kind(),
                    std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidInput
                )
            {
                return Err(error).context("sync Save Copy directory");
            }
            let mut saved = Vec::new();
            File::open(&destination)
                .context("open saved copy for readback")?
                .take((MAX_DRAFT_BYTES + 1) as u64)
                .read_to_end(&mut saved)
                .context("read back Save Copy")?;
            ensure!(
                saved == self.text.as_bytes(),
                "Save Copy bytes changed during persistence"
            );
            let validated = match self.source.kind {
                DocumentKind::Recipe => ValidatedDocument::Recipe(
                    recipe::load(&destination).context("reload saved recipe")?,
                ),
                DocumentKind::Suite => ValidatedDocument::Suite(
                    comparison::load(&destination).context("reload saved suite")?,
                ),
            };
            let digest = format!("{:x}", Sha256::digest(&saved));
            let (actual, reloaded_path) = match &validated {
                ValidatedDocument::Recipe(recipe) => (&recipe.digest, &recipe.path),
                ValidatedDocument::Suite(suite) => (&suite.digest, &suite.path),
            };
            ensure!(
                reloaded_path == &destination,
                "Save Copy path changed during reload"
            );
            ensure!(*actual == digest, "Save Copy changed during reload");
            Ok(validated)
        })()
        .with_context(|| format!("Save Copy left new file at {}", destination.display()))
    }
}

fn validate_at(kind: DocumentKind, path: &Path, bytes: &[u8]) -> Result<ValidatedDocument> {
    match kind {
        DocumentKind::Recipe => recipe::parse_bytes(path, bytes).map(ValidatedDocument::Recipe),
        DocumentKind::Suite => comparison::parse_bytes(path, bytes).map(ValidatedDocument::Suite),
    }
}
