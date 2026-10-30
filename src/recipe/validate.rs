use super::*;
use crate::model::{Checkpoint, ProfileName, ScenarioKind, StepName};
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct ValidatedRecipe {
    pub recipe: Recipe,
    pub path: PathBuf,
    pub project_root: PathBuf,
    pub base_artifact: Option<PathBuf>,
    pub target_artifact: PathBuf,
    /// SHA-256 of the recipe's original bytes, without host environment values.
    pub digest: String,
}
impl ValidatedRecipe {
    /// Declared scenario order, with the unchanged v1 clean/upgrade defaults.
    pub fn declared_scenarios(&self) -> Vec<ScenarioKind> {
        self.recipe
            .scenarios
            .clone()
            .unwrap_or_else(|| vec![ScenarioKind::Clean, ScenarioKind::Upgrade])
    }

    pub fn validate_selection(
        &self,
        profiles: &[ProfileName],
        scenarios: &[ScenarioKind],
    ) -> Result<()> {
        ensure!(
            !profiles.is_empty() && !scenarios.is_empty(),
            "select at least one profile and scenario"
        );
        let declared = self.declared_scenarios();
        for scenario in scenarios {
            ensure!(
                declared.contains(scenario),
                "scenario {scenario} is not declared by this recipe"
            );
        }
        for name in profiles {
            let profile = self
                .recipe
                .profiles
                .get(name)
                .with_context(|| format!("profile {name} is not defined"))?;
            for scenario in scenarios {
                let required = match scenario {
                    ScenarioKind::PluginLifecycle => {
                        ensure!(
                            *name == ProfileName::User,
                            "plugin_lifecycle requires user profile"
                        );
                        super::validate_plugin_contract(profile)?;
                        &super::plugin::STEPS[..]
                    }
                    ScenarioKind::InterruptedRollback => {
                        ensure!(
                            self.base_artifact.is_some(),
                            "interrupted_rollback requires artifacts.base"
                        );
                        super::interrupted_rollback::validate_profile(
                            profile,
                            &self.recipe.observe,
                            &declared,
                        )?;
                        &[
                            StepName::InstallBase,
                            StepName::Configure,
                            StepName::Exercise,
                            StepName::InterruptUpgrade,
                            StepName::Rollback,
                            StepName::ExerciseRolledBack,
                            StepName::Uninstall,
                        ][..]
                    }
                    ScenarioKind::Rollback => {
                        ensure!(
                            self.base_artifact.is_some(),
                            "rollback requires artifacts.base"
                        );
                        super::rollback::validate_profile(
                            profile,
                            &self.recipe.observe,
                            &declared,
                        )?;
                        &[
                            StepName::InstallBase,
                            StepName::Configure,
                            StepName::Exercise,
                            StepName::AttemptUpgrade,
                            StepName::Rollback,
                            StepName::ExerciseRolledBack,
                            StepName::Uninstall,
                        ][..]
                    }
                    ScenarioKind::Downgrade => {
                        ensure!(
                            self.base_artifact.is_some(),
                            "downgrade requires artifacts.base"
                        );
                        super::downgrade::validate_profile(profile)?;
                        if matches!(
                            profile.downgrade,
                            Some(DowngradeContract::Compatible { .. })
                        ) {
                            ensure!(
                                profile.steps.contains_key(&StepName::ExerciseDowngraded),
                                "{name}/downgrade requires step exercise_downgraded"
                            );
                        }
                        &[
                            StepName::InstallTarget,
                            StepName::Configure,
                            StepName::Exercise,
                            StepName::Downgrade,
                            StepName::Uninstall,
                        ][..]
                    }
                    ScenarioKind::Clean => &[
                        StepName::InstallTarget,
                        StepName::Configure,
                        StepName::Exercise,
                        StepName::Uninstall,
                    ][..],
                    ScenarioKind::Reinstall => {
                        ensure!(
                            profile
                                .checks
                                .iter()
                                .any(|check| check.checkpoint == Checkpoint::Reinstalled
                                    && matches!(check.expectation, Expectation::Unchanged { .. })),
                            "{name}/reinstall requires at least one unchanged contract"
                        );
                        &[
                            StepName::InstallTarget,
                            StepName::Configure,
                            StepName::Exercise,
                            StepName::Reinstall,
                            StepName::Uninstall,
                        ][..]
                    }
                    ScenarioKind::InterruptedInstall | ScenarioKind::InterruptedUpgrade => {
                        ensure!(
                            profile
                                .checks
                                .iter()
                                .any(|check| check.checkpoint == Checkpoint::Interrupted
                                    && check.expectation.path().is_some()),
                            "{name}/{scenario} requires an interrupted filesystem contract"
                        );
                        ensure!(profile.checks.iter().any(|check| check.checkpoint == Checkpoint::Recovered && matches!(&check.expectation, Expectation::Executable { stdout_contains: Some(value), .. } if !value.trim().is_empty())), "{name}/{scenario} requires a recovered Executable contract with nonempty stdout_contains");
                        if *scenario == ScenarioKind::InterruptedUpgrade {
                            ensure!(
                                self.base_artifact.is_some(),
                                "interrupted_upgrade requires artifacts.base"
                            );
                            ensure!(
                                profile.checks.iter().any(|check| check.checkpoint
                                    == Checkpoint::Recovered
                                    && match &check.expectation {
                                        Expectation::Unchanged { .. }
                                        | Expectation::Preserved { .. } => true,
                                        Expectation::Command {
                                            stdout_contains: Some(value),
                                            ..
                                        } => !value.trim().is_empty(),
                                        _ => false,
                                    }),
                                "{name}/{scenario} requires a recovered data contract"
                            );
                            &[
                                StepName::InstallBase,
                                StepName::InterruptUpgrade,
                                StepName::RecoverUpgrade,
                                StepName::Configure,
                                StepName::Exercise,
                                StepName::Uninstall,
                            ][..]
                        } else {
                            ensure!(
                                !profile.checks.iter().any(|check| matches!(
                                    check.checkpoint,
                                    Checkpoint::Interrupted | Checkpoint::Recovered
                                ) && matches!(
                                    check.expectation,
                                    Expectation::Preserved { .. }
                                )),
                                "preserved at interrupted/recovered requires interrupted_upgrade configuration"
                            );
                            &[
                                StepName::InterruptInstall,
                                StepName::RecoverInstall,
                                StepName::Configure,
                                StepName::Exercise,
                                StepName::Uninstall,
                            ][..]
                        }
                    }
                    ScenarioKind::Upgrade => {
                        ensure!(
                            self.base_artifact.is_some(),
                            "upgrade requires artifacts.base"
                        );
                        &[
                            StepName::InstallBase,
                            StepName::Configure,
                            StepName::Upgrade,
                            StepName::Exercise,
                            StepName::Uninstall,
                        ][..]
                    }
                };
                for step in required {
                    ensure!(
                        profile.steps.contains_key(step),
                        "{name}/{scenario} requires step {step}"
                    );
                }
                if crate::model::checkpoint_applies(*scenario, Checkpoint::TargetInstalled)
                    && *scenario != ScenarioKind::Upgrade
                {
                    ensure!(
                        !profile
                            .checks
                            .iter()
                            .any(|c| c.checkpoint == Checkpoint::TargetInstalled
                                && matches!(c.expectation, Expectation::Preserved { .. })),
                        "preserved at target_installed requires the upgrade scenario"
                    );
                }
            }
        }
        Ok(())
    }
}
pub fn load(path: &Path) -> Result<ValidatedRecipe> {
    let path = path.canonicalize().context("resolve recipe path")?;
    let bytes = fs::read(&path).context("read recipe")?;
    parse_bytes(&path, &bytes)
}

/// Validate exact recipe bytes against a source path in a canonical parent directory.
/// The source file itself need not exist, so adjacent drafts keep their original root.
pub fn parse_bytes(path: &Path, bytes: &[u8]) -> Result<ValidatedRecipe> {
    ensure!(path.is_absolute(), "recipe source path must be absolute");
    let project_root = path
        .parent()
        .context("recipe must have a parent directory")?
        .to_path_buf();
    ensure!(
        project_root
            .canonicalize()
            .context("resolve recipe source directory")?
            == project_root,
        "recipe source directory must be canonical"
    );
    let mut recipe: Recipe =
        toml::from_str(std::str::from_utf8(bytes)?).context("parse theseus.toml")?;
    match recipe.schema_version {
        1 => ensure!(
            recipe.scenarios.is_none(),
            "scenarios requires recipe schema version 2"
        ),
        2..=8 => {
            let scenarios = recipe.scenarios.as_ref().context(
                "recipe schema version 2/3/4/5/6/7/8 requires an explicit scenarios list",
            )?;
            ensure!(!scenarios.is_empty(), "declare at least one scenario");
            let mut seen = BTreeSet::new();
            for scenario in scenarios {
                ensure!(seen.insert(scenario), "duplicate scenario {scenario}");
            }
        }
        version => bail!("unsupported recipe schema version {version}"),
    }
    let plugin_declared = recipe
        .scenarios
        .as_ref()
        .is_some_and(|s| s.contains(&ScenarioKind::PluginLifecycle));
    ensure!(
        !plugin_declared || recipe.schema_version >= 7,
        "plugin_lifecycle requires recipe schema version 7"
    );
    ensure!(
        !plugin_declared || recipe.network == Network::None,
        "plugin_lifecycle requires network none"
    );
    let interrupted_rollback_declared = recipe
        .scenarios
        .as_ref()
        .is_some_and(|s| s.contains(&ScenarioKind::InterruptedRollback));
    ensure!(
        !interrupted_rollback_declared || recipe.schema_version >= 6,
        "interrupted_rollback requires recipe schema version 6"
    );
    let rollback_declared = recipe
        .scenarios
        .as_ref()
        .is_some_and(|s| s.contains(&ScenarioKind::Rollback));
    ensure!(
        !rollback_declared || recipe.schema_version >= 5,
        "rollback requires recipe schema version 5"
    );
    let downgrade_declared = recipe
        .scenarios
        .as_ref()
        .is_some_and(|scenarios| scenarios.contains(&ScenarioKind::Downgrade));
    ensure!(
        !downgrade_declared || recipe.schema_version >= 4,
        "downgrade requires recipe schema version 4"
    );
    if recipe.schema_version < 3 {
        ensure!(
            !recipe
                .scenarios
                .as_ref()
                .is_some_and(|scenarios| scenarios.iter().any(|kind| matches!(
                    kind,
                    ScenarioKind::InterruptedInstall | ScenarioKind::InterruptedUpgrade
                ))),
            "interruption requires recipe schema version 3"
        );
    }
    ensure!(
        !recipe.name.trim().is_empty() && !recipe.image.trim().is_empty(),
        "name and image cannot be empty"
    );
    ensure!(
        !recipe.profiles.is_empty(),
        "at least one profile is required"
    );
    ensure!(
        !recipe.observe.is_empty(),
        "at least one observed root is required"
    );
    ensure!(
        recipe.limits.max_entries > 0 && recipe.limits.max_content_bytes > 0,
        "observation limits must be positive"
    );
    for root in &mut recipe.observe {
        *root = normalize_container_path(root)?;
        ensure!(root != "/", "observing the entire filesystem is forbidden");
        ensure!(
            !["/proc", "/sys", "/dev"].iter().any(|p| contained(root, p)),
            "unsupported observed root {root}"
        );
    }
    for (index, root) in recipe.observe.iter().enumerate() {
        for other in &recipe.observe[..index] {
            ensure!(
                !contained(root, other) && !contained(other, root),
                "overlapping observed roots: {other} and {root}"
            );
        }
    }
    for (name, profile) in &mut recipe.profiles {
        let plugin_features = profile.plugin.is_some()
            || profile
                .steps
                .keys()
                .any(|s| matches!(s, StepName::DisablePlugin | StepName::EnablePlugin))
            || profile.checks.iter().any(|c| {
                matches!(
                    c.checkpoint,
                    Checkpoint::PluginDisabled | Checkpoint::PluginEnabled
                )
            });
        ensure!(
            !plugin_features || plugin_declared,
            "plugin features require declared plugin_lifecycle scenario"
        );
        if plugin_declared {
            ensure!(
                *name == ProfileName::User,
                "plugin_lifecycle requires user profile"
            );
            super::validate_plugin_contract(profile)?;
        }
        super::rollback::validate_features(
            profile,
            recipe.schema_version,
            rollback_declared,
            interrupted_rollback_declared,
        )?;
        ensure!(
            profile.interrupted_rollback.is_none() || recipe.schema_version >= 6,
            "interrupted_rollback requires recipe schema version 6"
        );
        ensure!(
            profile.interrupted_rollback.is_none() || interrupted_rollback_declared,
            "interrupted_rollback policy requires declared interrupted_rollback scenario"
        );
        let downgrade_features = profile.downgrade.is_some()
            || profile
                .steps
                .keys()
                .any(|step| matches!(step, StepName::Downgrade | StepName::ExerciseDowngraded))
            || profile.checks.iter().any(|check| {
                matches!(
                    check.checkpoint,
                    Checkpoint::Downgraded | Checkpoint::DowngradeRejected
                )
            });
        ensure!(
            !downgrade_features || recipe.schema_version >= 4,
            "downgrade requires recipe schema version 4"
        );
        ensure!(
            !downgrade_features || downgrade_declared,
            "downgrade features require declared downgrade scenario"
        );
        if recipe.schema_version == 1 {
            ensure!(
                !profile.steps.contains_key(&StepName::Reinstall),
                "reinstall requires recipe schema version 2"
            );
        }
        for (step, command) in &profile.steps {
            if matches!(
                step,
                StepName::InterruptInstall
                    | StepName::RecoverInstall
                    | StepName::InterruptUpgrade
                    | StepName::RecoverUpgrade
            ) {
                ensure!(
                    recipe.schema_version >= 3,
                    "interruption steps require recipe schema version 3"
                );
            }
            if matches!(
                step,
                StepName::InterruptInstall | StepName::InterruptUpgrade
            ) {
                ensure!(
                    !command.env.contains_key("THESEUS_INTERRUPT_BARRIER")
                        && !command
                            .pass_env
                            .iter()
                            .any(|name| name == "THESEUS_INTERRUPT_BARRIER"),
                    "THESEUS_INTERRUPT_BARRIER is reserved for interruption"
                );
            }
        }
        let mut ids = BTreeSet::new();
        for command in profile.steps.values() {
            validate_command(
                &command.script,
                command.timeout_seconds,
                &command.env,
                &command.pass_env,
            )?;
        }
        for check in &mut profile.checks {
            ensure!(
                !check.id.trim().is_empty() && ids.insert(check.id.clone()),
                "duplicate/empty check ID in {name}: {}",
                check.id
            );
            if recipe.schema_version == 1 {
                ensure!(
                    check.checkpoint != Checkpoint::Reinstalled
                        && !matches!(check.expectation, Expectation::Unchanged { .. }),
                    "reinstalled/unchanged requires recipe schema version 2"
                );
            }
            if matches!(
                check.checkpoint,
                Checkpoint::Interrupted | Checkpoint::Recovered
            ) {
                ensure!(
                    recipe.schema_version >= 3,
                    "interruption checkpoints require recipe schema version 3"
                );
                ensure!(
                    recipe
                        .scenarios
                        .as_ref()
                        .is_some_and(|scenarios| scenarios.iter().any(|scenario| {
                            crate::model::checkpoint_applies(*scenario, check.checkpoint)
                        })),
                    "interruption checkpoint requires a declared interrupted scenario"
                );
            }
            if check.checkpoint == Checkpoint::Reinstalled {
                ensure!(
                    recipe
                        .scenarios
                        .as_ref()
                        .is_some_and(|scenarios| scenarios.contains(&ScenarioKind::Reinstall)),
                    "reinstalled requires a recipe that must declare reinstall"
                );
            }
            if matches!(check.expectation, Expectation::Unchanged { .. }) {
                ensure!(
                    matches!(
                        check.checkpoint,
                        Checkpoint::Reinstalled
                            | Checkpoint::Interrupted
                            | Checkpoint::Recovered
                            | Checkpoint::Downgraded
                            | Checkpoint::DowngradeRejected
                            | Checkpoint::RolledBack
                    ),
                    "unchanged requires checkpoint reinstalled, interrupted, recovered, downgraded or downgrade_rejected"
                );
            }
            match &mut check.expectation {
                Expectation::McpStdio { probe } => {
                    ensure!(
                        recipe.schema_version >= 8,
                        "mcp_stdio requires recipe schema version 8"
                    );
                    probe.validate()?;
                }
                Expectation::JsonCommand {
                    script,
                    timeout_seconds,
                    env,
                    pass_env,
                    expected_exit,
                    max_stdout_bytes,
                    assertions,
                } => {
                    ensure!(
                        recipe.schema_version >= 7,
                        "json_command requires recipe schema version 7"
                    );
                    validate_command(script, *timeout_seconds, env, pass_env)?;
                    validate_exit(*expected_exit)?;
                    super::validate_json_assertions(assertions, *max_stdout_bytes)?;
                }
                Expectation::Command {
                    script,
                    timeout_seconds,
                    env,
                    pass_env,
                    expected_exit,
                    ..
                } => {
                    validate_command(script, *timeout_seconds, env, pass_env)?;
                    validate_exit(*expected_exit)?;
                }
                Expectation::Executable {
                    name,
                    resolved_path,
                    args,
                    timeout_seconds,
                    env,
                    pass_env,
                    expected_exit,
                    ..
                } => {
                    ensure!(
                        !name.is_empty()
                            && !name.contains('/')
                            && !name.contains('\0')
                            && !name.chars().any(char::is_whitespace),
                        "executable name must be a single command name"
                    );
                    *resolved_path = normalize_container_path(resolved_path)?;
                    ensure!(
                        !args.iter().any(|arg| arg.contains('\0')),
                        "executable argument contains NUL"
                    );
                    validate_command("executable", *timeout_seconds, env, pass_env)?;
                    validate_exit(*expected_exit)?;
                }
                Expectation::Absent { path }
                | Expectation::Present { path, .. }
                | Expectation::Preserved { path }
                | Expectation::Unchanged { path }
                | Expectation::Changed { path } => {
                    *path = normalize_container_path(path)?;
                    ensure!(
                        recipe.observe.iter().any(|root| contained(path, root)),
                        "check {} is outside observed roots",
                        check.id
                    );
                }
            }
            if matches!(check.expectation, Expectation::Preserved { .. }) {
                ensure!(
                    matches!(
                        check.checkpoint,
                        Checkpoint::TargetInstalled
                            | Checkpoint::Uninstalled
                            | Checkpoint::Reinstalled
                            | Checkpoint::Interrupted
                            | Checkpoint::Recovered
                            | Checkpoint::Downgraded
                            | Checkpoint::DowngradeRejected
                    ),
                    "preserved requires a checkpoint after configuration"
                );
            }
        }
    }
    for profile in recipe.profiles.values() {
        if rollback_declared {
            super::rollback::validate_profile(
                profile,
                &recipe.observe,
                recipe.scenarios.as_deref().unwrap_or_default(),
            )?;
        }
        if interrupted_rollback_declared {
            super::interrupted_rollback::validate_profile(
                profile,
                &recipe.observe,
                recipe.scenarios.as_deref().unwrap_or_default(),
            )?;
        }
        if profile.downgrade.is_some() {
            super::downgrade::validate_profile(profile)?;
        }
    }
    let target_artifact = resolve_artifact(&project_root, &recipe.artifacts.target)?;
    let base_artifact = recipe
        .artifacts
        .base
        .as_ref()
        .map(|p| resolve_artifact(&project_root, p))
        .transpose()?;
    Ok(ValidatedRecipe {
        recipe,
        path: path.to_path_buf(),
        project_root,
        base_artifact,
        target_artifact,
        digest: format!("{:x}", Sha256::digest(bytes)),
    })
}
fn validate_exit(status: i32) -> Result<()> {
    ensure!(
        (0..=255).contains(&status),
        "expected_exit must be between 0 and 255"
    );
    Ok(())
}
fn validate_command(
    script: &str,
    timeout: u64,
    env: &std::collections::BTreeMap<String, String>,
    pass: &[String],
) -> Result<()> {
    ensure!(
        !script.trim().is_empty() && !script.contains('\0'),
        "script cannot be empty or contain NUL"
    );
    ensure!(
        (1..=MAX_TIMEOUT_SECONDS).contains(&timeout),
        "timeout must be between 1 and {MAX_TIMEOUT_SECONDS} seconds"
    );
    let mut seen = BTreeSet::new();
    for name in env.keys().chain(pass) {
        ensure!(valid_env_name(name), "invalid environment name {name}");
        ensure!(
            seen.insert(name),
            "duplicate/conflicting environment name {name}"
        );
    }
    ensure!(
        !env.values().any(|v| v.contains('\0')),
        "environment value contains NUL"
    );
    Ok(())
}
pub(crate) fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
/// Normalize absolute container paths component-wise; traversal is always rejected.
pub fn normalize_container_path(path: &str) -> Result<String> {
    ensure!(
        path.starts_with('/') && !path.contains('\0'),
        "container path must be absolute and NUL-free: {path:?}"
    );
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => bail!("path traversal is forbidden: {path}"),
            value => components.push(value),
        }
    }
    Ok(format!("/{}", components.join("/")))
}
pub(crate) fn contained(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}
fn resolve_artifact(project: &Path, relative: &Path) -> Result<PathBuf> {
    ensure!(
        !relative.as_os_str().is_empty()
            && !relative.is_absolute()
            && !relative
                .components()
                .any(|c| matches!(c, Component::ParentDir)),
        "artifact must be a project-relative path without traversal"
    );
    let path = project
        .join(relative)
        .canonicalize()
        .with_context(|| format!("resolve artifact {}", relative.display()))?;
    ensure!(
        path.starts_with(project),
        "artifact escapes the project root"
    );
    validate_tree(&path, project, &mut BTreeSet::new())?;
    Ok(path)
}
fn validate_tree(path: &Path, project: &Path, ancestors: &mut BTreeSet<PathBuf>) -> Result<()> {
    let resolved = path
        .canonicalize()
        .with_context(|| format!("resolve artifact entry {}", path.display()))?;
    ensure!(
        resolved.starts_with(project),
        "artifact symlink escapes the project: {}",
        path.display()
    );
    let metadata = fs::metadata(&resolved)?;
    ensure!(
        metadata.is_file() || metadata.is_dir(),
        "artifact contains unsupported special file: {}",
        path.display()
    );
    if metadata.is_dir() {
        ensure!(
            ancestors.insert(resolved.clone()),
            "artifact contains a directory symlink cycle"
        );
        for entry in fs::read_dir(&resolved)? {
            validate_tree(&entry?.path(), project, ancestors)?;
        }
        ancestors.remove(&resolved);
    }
    Ok(())
}
