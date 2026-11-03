//! Declarative catalog-only native recipes; runtime authority is never recipe input.
use super::model::*;
use crate::model::{Checkpoint, ScenarioKind, StepName};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeOs {
    Linux,
    Macos,
    Windows,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRunLimits {
    pub run_seconds: u64,
    pub inspection_seconds: u64,
    pub max_operations: u32,
    pub output_bytes: u64,
}
impl Default for NativeRunLimits {
    fn default() -> Self {
        Self {
            run_seconds: 30,
            inspection_seconds: 30,
            max_operations: 128,
            output_bytes: 16384,
        }
    }
}
impl NativeRunLimits {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=30).contains(&self.run_seconds)
                && (1..=30).contains(&self.inspection_seconds)
                && (1..=128).contains(&self.max_operations)
                && (1..=16384).contains(&self.output_bytes),
            "unsupported native limits"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeArtifact {
    pub length: u64,
    pub sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLeaves {
    pub reader: String,
    pub app: String,
    pub data: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCheck {
    pub id: String,
    pub checkpoint: Checkpoint,
    pub command: CatalogCommand,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRecipe {
    pub kind: String,
    pub schema: u32,
    pub os: NativeOs,
    pub manifest_digest: String,
    pub leaves: NativeLeaves,
    pub base: NativeArtifact,
    pub target: NativeArtifact,
    pub scenarios: Vec<ScenarioKind>,
    pub steps: BTreeMap<StepName, CatalogCommand>,
    pub checks: Vec<NativeCheck>,
    pub limits: NativeRunLimits,
}
#[derive(Debug, Clone)]
pub struct ValidatedNativeRecipe {
    pub recipe: NativeRecipe,
    pub path: PathBuf,
    pub digest: String,
}
impl NativeRecipe {
    pub fn validate(&self) -> Result<()> {
        envelope(&self.kind, self.schema, "theseus_native_recipe")?;
        digest(&self.manifest_digest)?;
        self.limits.validate()?;
        ensure!(
            self.leaves.reader
                == if self.os == NativeOs::Windows {
                    "reader.exe"
                } else {
                    "reader"
                }
                && self.leaves.app == "app"
                && self.leaves.data == "data",
            "native recipe requires exact three declared leaves"
        );
        for artifact in [&self.base, &self.target] {
            ensure!(
                (1..=64 * 1024 * 1024).contains(&artifact.length),
                "unsupported native reader size"
            );
            digest(&artifact.sha256)?;
        }
        ensure!(!self.scenarios.is_empty(), "empty native selection");
        let mut scenarios = BTreeSet::new();
        for scenario in &self.scenarios {
            ordinary(*scenario)?;
            ensure!(scenarios.insert(scenario), "duplicate native scenario");
        }
        let mut required = BTreeMap::from([
            (StepName::InstallTarget, CatalogProgram::InstallTarget),
            (StepName::Configure, CatalogProgram::Configure),
            (StepName::Exercise, CatalogProgram::Exercise),
            (StepName::Uninstall, CatalogProgram::Uninstall),
        ]);
        if scenarios.contains(&ScenarioKind::Upgrade) {
            required.insert(StepName::InstallBase, CatalogProgram::InstallBase);
            required.insert(StepName::Upgrade, CatalogProgram::InstallTarget);
        }
        if scenarios.contains(&ScenarioKind::Reinstall) {
            required.insert(StepName::Reinstall, CatalogProgram::InstallTarget);
        }
        ensure!(
            self.steps.len() == required.len(),
            "unexpected/missing native step mapping"
        );
        let mut ids = BTreeSet::new();
        for (step, program) in required {
            let command = self
                .steps
                .get(&step)
                .ok_or_else(|| anyhow::anyhow!("missing native step {step}"))?;
            command.validate()?;
            ensure!(
                command.program == program,
                "invalid native step catalog mapping"
            );
            ensure!(ids.insert(&command.id), "duplicate command ID");
        }
        for check in &self.checks {
            text(&check.id, 128)?;
            check.command.validate()?;
            ensure!(
                check.id == check.command.id && ids.insert(&check.id),
                "invalid/duplicate native check ID"
            );
            ensure!(
                matches!(
                    check.command.program,
                    CatalogProgram::CheckBase
                        | CatalogProgram::CheckData
                        | CatalogProgram::CheckTarget
                ),
                "native check must name fixed check program"
            );
            ensure!(
                check.command.stdout_literal.is_some(),
                "native reader check requires literal output evidence"
            );
            ensure!(
                self.scenarios
                    .iter()
                    .any(|s| crate::model::checkpoint_applies(*s, check.checkpoint)),
                "inapplicable native checkpoint"
            );
        }
        for scenario in &self.scenarios {
            let mut required_checks = vec![
                (Checkpoint::TargetInstalled, CatalogProgram::CheckTarget),
                (Checkpoint::Configured, CatalogProgram::CheckData),
            ];
            if *scenario == ScenarioKind::Upgrade {
                required_checks.push((Checkpoint::BaseInstalled, CatalogProgram::CheckBase));
                required_checks.push((Checkpoint::TargetInstalled, CatalogProgram::CheckData));
            }
            if *scenario == ScenarioKind::Reinstall {
                required_checks.push((Checkpoint::Reinstalled, CatalogProgram::CheckTarget));
                required_checks.push((Checkpoint::Reinstalled, CatalogProgram::CheckData));
            }
            for (checkpoint, program) in required_checks {
                ensure!(
                    self.checks
                        .iter()
                        .any(|c| c.checkpoint == checkpoint && c.command.program == program),
                    "missing required native reader/data check at {checkpoint}"
                );
            }
        }
        Ok(())
    }
}
impl ValidatedNativeRecipe {
    pub fn artifact_commitment(&self) -> Result<theseus_native_protocol::v2::Digest> {
        use theseus_native_protocol::v2;
        Ok(v2::artifact_commitment(
            self.recipe.base.length,
            v2::Digest::parse(&self.recipe.base.sha256)?,
            self.recipe.target.length,
            v2::Digest::parse(&self.recipe.target.sha256)?,
        )?)
    }
    pub fn decode(bytes: &[u8], path: PathBuf) -> Result<Self> {
        let recipe: NativeRecipe = decode(bytes)?;
        recipe.validate()?;
        Ok(Self {
            recipe,
            path,
            digest: format!("{:x}", Sha256::digest(bytes)),
        })
    }
    pub fn validate_selection(
        &self,
        scenarios: &[ScenarioKind],
        limits: &NativeRunLimits,
    ) -> Result<()> {
        self.recipe.validate()?;
        limits.validate()?;
        ensure!(!scenarios.is_empty(), "empty native selection");
        let mut seen = BTreeSet::new();
        for scenario in scenarios {
            ordinary(*scenario)?;
            ensure!(
                self.recipe.scenarios.contains(scenario) && seen.insert(scenario),
                "undeclared/duplicate native scenario"
            );
        }
        ensure!(
            limits.run_seconds <= self.recipe.limits.run_seconds
                && limits.inspection_seconds <= self.recipe.limits.inspection_seconds
                && limits.max_operations <= self.recipe.limits.max_operations
                && limits.output_bytes <= self.recipe.limits.output_bytes,
            "native selection exceeds recipe limits"
        );
        Ok(())
    }
}
