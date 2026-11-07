//! Bounded manifest validation; offline structural checks never open recipe paths.
use super::model::*;
use crate::recipe::{ValidatedRecipe, normalize_container_path, validate::contained};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct ValidatedSuite {
    pub manifest: SuiteManifest,
    pub variants: Vec<ValidatedVariant>,
    pub project_root: PathBuf,
    pub path: PathBuf,
    /// SHA-256 of the manifest's original bytes.
    pub digest: String,
}
#[derive(Debug, Clone)]
pub struct ValidatedVariant {
    pub id: String,
    pub recipe: ValidatedRecipe,
}
fn unique<T: Ord>(values: &[T]) -> bool {
    values.iter().collect::<BTreeSet<_>>().len() == values.len()
}
fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
/// Validate bounds, identity and matrix without accessing any filesystem.
/// Paths in an already-parsed manifest must be normalized (load normalizes them).
pub fn validate_manifest(suite: &SuiteManifest) -> Result<()> {
    ensure!(
        suite.schema_version == COMPARISON_SCHEMA_VERSION,
        "unsupported comparison schema version {}",
        suite.schema_version
    );
    ensure!(
        !suite.name.trim().is_empty() && suite.name.chars().count() <= 128,
        "suite name must contain 1..128 characters"
    );
    ensure!(
        (2..=16).contains(&suite.variants.len()),
        "suite requires 2..16 variants"
    );
    ensure!(
        (1..=32).contains(&suite.scopes.len()),
        "suite requires 1..32 scopes"
    );
    ensure!(
        (1..=32).contains(&suite.stages.len()),
        "suite requires 1..32 stages"
    );
    ensure!(
        (1..=2).contains(&suite.profiles.len()) && unique(&suite.profiles),
        "select unique nonempty profiles"
    );
    ensure!(
        (1..=8).contains(&suite.scenarios.len()) && unique(&suite.scenarios),
        "select unique nonempty scenarios"
    );
    ensure!(
        unique(&suite.stages)
            && suite
                .stages
                .iter()
                .all(|s| !s.trim().is_empty() && s.len() <= 128),
        "stages must be unique nonempty names of at most 128 bytes"
    );
    let mut ids = BTreeSet::new();
    for variant in &suite.variants {
        ensure!(
            valid_id(&variant.id) && ids.insert(&variant.id),
            "invalid or duplicate variant ID {}",
            variant.id
        );
        ensure!(
            !variant.recipe.as_os_str().is_empty(),
            "recipe path cannot be empty"
        );
    }
    ensure!(
        ids.contains(&suite.baseline),
        "baseline must name a declared variant"
    );
    let mut ids = BTreeSet::new();
    for scope in &suite.scopes {
        ensure!(
            valid_id(&scope.id) && ids.insert(&scope.id),
            "invalid or duplicate scope ID {}",
            scope.id
        );
        ensure!(
            suite.profiles.contains(&scope.profile),
            "scope profile {} is not selected",
            scope.profile
        );
        ensure!(
            scope.path != "/" && normalize_container_path(&scope.path)? == scope.path,
            "scope path must be normalized absolute application path"
        );
    }
    // Each scope has exactly one selected profile, so no extra profile multiplier.
    let cells = (suite.variants.len() - 1)
        .checked_mul(suite.scenarios.len())
        .and_then(|n| n.checked_mul(suite.stages.len()))
        .and_then(|n| n.checked_mul(suite.scopes.len()));
    ensure!(
        cells.is_some_and(|n| n <= MAX_COMPARISON_CELLS),
        "suite exceeds {MAX_COMPARISON_CELLS} comparison cells"
    );
    Ok(())
}
/// Load every recipe and validate all selected combinations before any execution.
pub fn load(path: &Path) -> Result<ValidatedSuite> {
    let path = path.canonicalize().context("resolve suite manifest path")?;
    let mut bytes = Vec::new();
    File::open(&path)
        .context("open suite manifest")?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("read suite manifest")?;
    ensure!(
        bytes.len() as u64 <= MAX_MANIFEST_BYTES,
        "suite manifest exceeds 1 MiB"
    );
    parse_bytes(&path, &bytes)
}

/// Validate exact manifest bytes relative to a source path in a canonical parent.
pub fn parse_bytes(path: &Path, bytes: &[u8]) -> Result<ValidatedSuite> {
    ensure!(path.is_absolute(), "suite source path must be absolute");
    let project_root = path
        .parent()
        .context("manifest must have a parent directory")?
        .to_path_buf();
    ensure!(
        project_root
            .canonicalize()
            .context("resolve suite source directory")?
            == project_root,
        "suite source directory must be canonical"
    );
    ensure!(
        bytes.len() as u64 <= MAX_MANIFEST_BYTES,
        "suite manifest exceeds 1 MiB"
    );
    let mut manifest: SuiteManifest =
        toml::from_str(std::str::from_utf8(bytes)?).context("parse comparison suite")?;
    for scope in &mut manifest.scopes {
        scope.path = normalize_container_path(&scope.path)?;
    }
    validate_manifest(&manifest)?;
    let mut variants = Vec::with_capacity(manifest.variants.len());
    for variant in &manifest.variants {
        let selected = crate::native::storage::load_recipe(&project_root.join(&variant.recipe))
            .with_context(|| format!("load variant {}", variant.id))?;
        let crate::engine::domain::SelectedRecipe::Docker(recipe) = selected else {
            anyhow::bail!(
                "native comparisons are unsupported (variant {})",
                variant.id
            );
        };
        recipe
            .validate_selection(&manifest.profiles, &manifest.scenarios)
            .with_context(|| format!("variant {} matrix", variant.id))?;
        for scope in &manifest.scopes {
            ensure!(
                recipe
                    .recipe
                    .observe
                    .iter()
                    .filter(|root| contained(&scope.path, root))
                    .count()
                    == 1,
                "variant {} does not uniquely observe scope {}",
                variant.id,
                scope.id
            );
        }
        variants.push(ValidatedVariant {
            id: variant.id.clone(),
            recipe,
        });
    }
    Ok(ValidatedSuite {
        manifest,
        variants,
        project_root,
        path: path.to_path_buf(),
        digest: format!("{:x}", Sha256::digest(bytes)),
    })
}
