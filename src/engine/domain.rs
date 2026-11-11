//! Closed backend selection and effect-free lifecycle compilation.
use super::RunOptions;
use crate::{
    model::{ProfileName, RunJournal, RunReport, ScenarioKind},
    native::{
        model::{CatalogCommand, NativeJournal, NativeReport},
        recipe::{NativeRunLimits, ValidatedNativeRecipe},
    },
    recipe::ValidatedRecipe,
    runner::CommandRequest,
};
use anyhow::{Result, bail};
use std::path::PathBuf;

#[derive(Debug, Clone)]
// Frozen closed metadata sum; variants carry schema DTOs, never native owners.
#[allow(clippy::large_enum_variant)]
pub enum SelectedRecipe {
    Docker(ValidatedRecipe),
    Native(ValidatedNativeRecipe),
    NativeIntegration(crate::native::integration::ValidatedIntegrationRecipe),
}
#[derive(Debug, Clone)]
// Frozen closed metadata sum; variants carry schema DTOs, never native owners.
#[allow(clippy::large_enum_variant)]
pub enum StoredRun {
    Docker(RunReport),
    Native(NativeReport),
    NativeIntegration(crate::native::integration::NativeIntegrationReport),
}
#[derive(Debug, Clone)]
// Frozen closed metadata sum; variants carry schema DTOs, never native owners.
#[allow(clippy::large_enum_variant)]
pub enum StoredJournal {
    Docker(RunJournal),
    Native(NativeJournal),
    NativeIntegration(crate::native::integration::NativeIntegrationJournal),
}
#[derive(Debug, Clone)]
pub struct NativeRunOptions {
    pub output_root: PathBuf,
    pub scenarios: Vec<ScenarioKind>,
    pub limits: NativeRunLimits,
}
pub type NativeIntegrationRunOptions = NativeRunOptions;
#[derive(Debug, Clone)]
pub enum SelectedRunOptions {
    Docker(RunOptions),
    Native(NativeRunOptions),
    NativeIntegration(NativeIntegrationRunOptions),
}
pub struct SelectedRunResult {
    pub report: StoredRun,
    pub report_path: PathBuf,
}
#[derive(Debug, Clone)]
pub enum StageCommand {
    Docker(CommandRequest),
    Native(CatalogCommand),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleRole {
    Docker(ProfileName),
    NativeCurrentAccount,
}
#[derive(Debug, Clone)]
pub struct ScenarioPlan {
    pub kind: ScenarioKind,
    pub role: LifecycleRole,
}
#[derive(Debug, Clone)]
pub struct LifecyclePlan {
    pub source_digest: String,
    pub scenarios: Vec<ScenarioPlan>,
}
/// Admission is pure: mismatched backends refuse before any store/provider/lease effect.
pub fn compile(recipe: &SelectedRecipe, selection: &SelectedRunOptions) -> Result<LifecyclePlan> {
    let scenarios = match (recipe, selection) {
        (SelectedRecipe::Docker(recipe), SelectedRunOptions::Docker(options)) => {
            recipe.validate_selection(&options.profiles, &options.scenarios)?;
            anyhow::ensure!(
                options.log_limit > 0 && options.log_limit <= crate::recipe::DEFAULT_LOG_LIMIT,
                "invalid log limit"
            );
            anyhow::ensure!(
                options
                    .profiles
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == options.profiles.len()
                    && options
                        .scenarios
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        == options.scenarios.len(),
                "duplicate matrix selection"
            );
            options
                .profiles
                .iter()
                .flat_map(|role| {
                    options.scenarios.iter().map(move |kind| ScenarioPlan {
                        kind: *kind,
                        role: LifecycleRole::Docker(*role),
                    })
                })
                .collect()
        }
        (SelectedRecipe::Native(recipe), SelectedRunOptions::Native(options)) => {
            recipe.validate_selection(&options.scenarios, &options.limits)?;
            options
                .scenarios
                .iter()
                .map(|kind| ScenarioPlan {
                    kind: *kind,
                    role: LifecycleRole::NativeCurrentAccount,
                })
                .collect()
        }
        (
            SelectedRecipe::NativeIntegration(recipe),
            SelectedRunOptions::NativeIntegration(options),
        ) => {
            recipe.validate_selection(&options.scenarios, &options.limits)?;
            options
                .scenarios
                .iter()
                .map(|kind| ScenarioPlan {
                    kind: *kind,
                    role: LifecycleRole::NativeCurrentAccount,
                })
                .collect()
        }
        _ => bail!("recipe/options backend mismatch"),
    };
    Ok(LifecyclePlan {
        source_digest: match recipe {
            SelectedRecipe::Docker(r) => r.digest.clone(),
            SelectedRecipe::Native(r) => r.digest.clone(),
            SelectedRecipe::NativeIntegration(r) => r.digest.clone(),
        },
        scenarios,
    })
}
