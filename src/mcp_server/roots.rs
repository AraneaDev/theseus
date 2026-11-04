//! Retained startup directory authority and bounded recipe acquisition.
use super::types::{ErrorCode, InspectArgs, InspectSelector, QueryError, ToolCall};
use crate::report::ReportStore;
use std::{
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use theseus_host::fs::{HostDir, RelativePath, RootPolicy, fingerprint};

pub(crate) struct Roots {
    pub native_target: Option<Arc<crate::native::launch::LaunchAuthority>>,
    pub project: Arc<HostDir>,
    pub output: Arc<ReportStore>,
    pub project_path: PathBuf,
}
pub(super) fn error(code: ErrorCode) -> QueryError {
    QueryError::new(code, anyhow::anyhow!("Recipe query rejected"))
}
fn io_error(source: io::Error) -> QueryError {
    // Native no-follow opens classify unsafe file types as InvalidInput or a
    // platform ELOOP/ENOTDIR. Retain the original native error in every case.
    let unsafe_entry = matches!(
        source.kind(),
        io::ErrorKind::InvalidInput | io::ErrorKind::NotADirectory | io::ErrorKind::IsADirectory
    ) || (cfg!(target_os = "linux") && source.raw_os_error() == Some(40))
        || (cfg!(target_os = "macos") && source.raw_os_error() == Some(62));
    QueryError::new(
        if unsafe_entry {
            ErrorCode::UnsafePath
        } else {
            ErrorCode::IoError
        },
        source.into(),
    )
}
fn absolute(path: &Path) -> Result<PathBuf, QueryError> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().map_err(io_error)?.join(path)
    };
    let mut resolved = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => (),
            Component::ParentDir => return Err(error(ErrorCode::UnsafePath)),
            other => resolved.push(other.as_os_str()),
        }
    }
    Ok(resolved)
}
impl Roots {
    pub fn open(project: &Path, output: &Path) -> Result<Self, QueryError> {
        let project_path = absolute(project)?;
        // Resolve lexically first; canonicalize only after no-follow acquisition.
        let project = Arc::new(
            HostDir::open(&project_path, RootPolicy::AllComponentsNoFollow).map_err(io_error)?,
        );
        let canonical = project_path.canonicalize().map_err(io_error)?;
        let reopened =
            HostDir::open(&canonical, RootPolicy::AllComponentsNoFollow).map_err(io_error)?;
        if !project.same_object(&reopened).map_err(io_error)? {
            return Err(error(ErrorCode::RootChanged));
        }
        let output = Arc::new(
            ReportStore::safe_root(&absolute(output)?, true)
                .map_err(|e| QueryError::new(ErrorCode::UnsafePath, e))?,
        );
        Ok(Self {
            native_target: None,
            project,
            output,
            project_path: canonical,
        })
    }
    fn verify_root(&self) -> Result<(), QueryError> {
        let current = HostDir::open(&self.project_path, RootPolicy::AllComponentsNoFollow)
            .map_err(|e| QueryError::new(ErrorCode::RootChanged, e.into()))?;
        if !self.project.same_object(&current).map_err(io_error)? {
            return Err(error(ErrorCode::RootChanged));
        }
        Ok(())
    }
    fn verify_parent(
        &self,
        relative_parent: Option<&RelativePath>,
        retained: &HostDir,
        canonical: &Path,
    ) -> Result<(), QueryError> {
        self.verify_root()?;
        let current = match relative_parent {
            Some(path) => self.project.open_child(path),
            None => HostDir::open(&self.project_path, RootPolicy::AllComponentsNoFollow),
        }
        .map_err(|e| QueryError::new(ErrorCode::RootChanged, e.into()))?;
        let named = HostDir::open(canonical, RootPolicy::AllComponentsNoFollow)
            .map_err(|e| QueryError::new(ErrorCode::RootChanged, e.into()))?;
        if !retained.same_object(&current).map_err(io_error)?
            || !retained.same_object(&named).map_err(io_error)?
        {
            return Err(error(ErrorCode::RootChanged));
        }
        Ok(())
    }
}
#[cfg(test)]
pub(crate) fn read_recipe(
    roots: &Roots,
    path: &str,
) -> Result<crate::recipe::ValidatedRecipe, QueryError> {
    match read_selected_recipe(roots, path)? {
        crate::engine::domain::SelectedRecipe::Docker(recipe) => Ok(recipe),
        crate::engine::domain::SelectedRecipe::Native(_)
        | crate::engine::domain::SelectedRecipe::NativeIntegration(_) => {
            Err(error(ErrorCode::RecipeInvalid))
        }
    }
}
pub(crate) fn read_selected_recipe(
    roots: &Roots,
    path: &str,
) -> Result<crate::engine::domain::SelectedRecipe, QueryError> {
    // Reuse the public portable path rule as well as RelativePath's native rule.
    let args = InspectArgs {
        recipe_path: path.into(),
        recipe_digest: None,
        selector: InspectSelector::Overview,
        limit: None,
        cursor: None,
    };
    ToolCall::RecipeInspect(args)
        .validate()
        .map_err(|_| error(ErrorCode::UnsafePath))?;
    let relative = RelativePath::parse(path).map_err(io_error)?;
    roots.verify_root()?;
    let parent_path = path
        .rsplit_once('/')
        .map(|(p, _)| RelativePath::parse(p))
        .transpose()
        .map_err(io_error)?;
    let parent = match &parent_path {
        Some(p) => roots.project.open_child(p),
        None => HostDir::open(&roots.project_path, RootPolicy::AllComponentsNoFollow),
    }
    .map_err(io_error)?;
    let source_path = roots.project_path.join(path);
    let canonical_parent = source_path
        .parent()
        .expect("portable path has parent")
        .canonicalize()
        .map_err(io_error)?;
    roots.verify_parent(parent_path.as_ref(), &parent, &canonical_parent)?;
    let mut file = roots.project.open_file(&relative).map_err(io_error)?;
    let before = fingerprint(&file).map_err(io_error)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > 1_048_576 {
        return Err(error(ErrorCode::SourceTooLarge));
    }
    roots.verify_parent(parent_path.as_ref(), &parent, &canonical_parent)?;
    let after = fingerprint(&file).map_err(io_error)?;
    let reopened = roots.project.open_file(&relative).map_err(io_error)?;
    if before != after || before != fingerprint(&reopened).map_err(io_error)? {
        return Err(error(ErrorCode::RecipeChanged));
    }
    roots.verify_parent(parent_path.as_ref(), &parent, &canonical_parent)?;
    let parsed = crate::native::storage::decode_recipe(
        &bytes,
        canonical_parent.join(source_path.file_name().expect("portable leaf")),
    );
    // The parser's artifact validation uses pathname context. Validate that
    // context even on parse failure, retaining the more specific namespace error.
    roots.verify_parent(parent_path.as_ref(), &parent, &canonical_parent)?;
    let reopened = roots.project.open_file(&relative).map_err(io_error)?;
    if before != fingerprint(&file).map_err(io_error)?
        || before != fingerprint(&reopened).map_err(io_error)?
    {
        return Err(error(ErrorCode::RecipeChanged));
    }
    parsed.map_err(|e| QueryError::new(ErrorCode::RecipeInvalid, e))
}
#[cfg(test)]
#[path = "roots_tests.rs"]
mod tests;
