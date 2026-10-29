use anyhow::{Result, ensure};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedFailureContract {
    pub expected_exit: i32,
    pub stdout_contains: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RollbackRecoveryContract {
    pub change_checks: Vec<String>,
    pub binary_checks: Vec<String>,
    pub runtime_checks: Vec<String>,
    pub data_checks: Vec<String>,
    pub restoration: RestorationPolicy,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RollbackContract {
    pub failure: ExpectedFailureContract,
    #[serde(flatten)]
    pub recovery: RollbackRecoveryContract,
}
impl<'de> Deserialize<'de> for RollbackContract {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        // Keep schema-5 keys flat and strict: flatten with deny_unknown_fields
        // cannot reliably distinguish the failure field from recovery fields.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireContract {
            failure: ExpectedFailureContract,
            change_checks: Vec<String>,
            binary_checks: Vec<String>,
            runtime_checks: Vec<String>,
            data_checks: Vec<String>,
            restoration: RestorationPolicy,
        }
        let wire = WireContract::deserialize(deserializer)?;
        Ok(Self {
            failure: wire.failure,
            recovery: RollbackRecoveryContract {
                change_checks: wire.change_checks,
                binary_checks: wire.binary_checks,
                runtime_checks: wire.runtime_checks,
                data_checks: wire.data_checks,
                restoration: wire.restoration,
            },
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum RestorationPolicy {
    BinaryOnly,
    RestoreData {
        backup_root: String,
        scopes: Vec<BackupScope>,
    },
}
impl<'de> Deserialize<'de> for RestorationPolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        // Serde's tagged unit variant ignores extra fields, even under
        // deny_unknown_fields. An empty struct variant enforces the same strict
        // policy as RestoreData while retaining the public unit variant.
        #[derive(Deserialize)]
        #[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
        enum StrictPolicy {
            BinaryOnly {},
            RestoreData {
                backup_root: String,
                scopes: Vec<BackupScope>,
            },
        }
        Ok(match StrictPolicy::deserialize(deserializer)? {
            StrictPolicy::BinaryOnly {} => Self::BinaryOnly,
            StrictPolicy::RestoreData {
                backup_root,
                scopes,
            } => Self::RestoreData {
                backup_root,
                scopes,
            },
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupScope {
    pub id: String,
    pub source: String,
    pub stored: String,
}

/// Validate policy values only. Check IDs, observation coverage, reference kinds
/// and binary/data path overlap require profile context and are resolved later.
pub fn validate_rollback_contract(contract: &RollbackContract) -> Result<()> {
    ensure!(
        (1..=125).contains(&contract.failure.expected_exit),
        "rollback expected_exit must be in 1..=125"
    );
    ensure!(
        !contract.failure.stdout_contains.trim().is_empty(),
        "rollback requires nonempty stdout_contains"
    );
    validate_rollback_recovery(&contract.recovery)
}

/// Validate shared recovery values without profile, checkpoint or runtime context.
pub fn validate_rollback_recovery(contract: &RollbackRecoveryContract) -> Result<()> {
    let mut references = BTreeSet::new();
    for ids in [
        &contract.change_checks,
        &contract.binary_checks,
        &contract.runtime_checks,
        &contract.data_checks,
    ] {
        ensure!(
            !ids.is_empty(),
            "rollback requires nonempty check reference lists"
        );
        for id in ids {
            ensure!(
                !id.trim().is_empty(),
                "rollback check reference IDs must be nonempty"
            );
            ensure!(
                references.insert(id),
                "rollback check reference IDs must be unique across roles"
            );
        }
    }
    if let RestorationPolicy::RestoreData {
        backup_root,
        scopes,
    } = &contract.restoration
    {
        validate_absolute(backup_root)?;
        ensure!(
            !["/inputs", "/proc", "/sys", "/dev"]
                .iter()
                .any(|root| super::validate::contained(backup_root, root)),
            "rollback backup root must not be under inputs or special filesystems"
        );
        ensure!(
            (1..=64).contains(&scopes.len()),
            "rollback requires 1..=64 backup scopes"
        );
        let mut ids = BTreeSet::new();
        for (index, scope) in scopes.iter().enumerate() {
            ensure!(
                (1..=64).contains(&scope.id.len())
                    && scope
                        .id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')),
                "rollback scope IDs require 1..=64 ASCII letters/digits/_/-"
            );
            ensure!(ids.insert(&scope.id), "rollback scope IDs must be unique");
            validate_absolute(&scope.source)?;
            ensure!(
                !scope.stored.is_empty()
                    && !scope.stored.contains('\0')
                    && scope
                        .stored
                        .split('/')
                        .all(|part| !part.is_empty() && !matches!(part, "." | "..")),
                "rollback stored scope must be a normalized nonempty relative path"
            );
            ensure!(
                !overlap(&scope.source, backup_root),
                "rollback backup root and source scopes must not overlap"
            );
            for other in &scopes[..index] {
                ensure!(
                    !overlap(&scope.source, &other.source),
                    "rollback source scopes must not overlap"
                );
                ensure!(
                    !overlap(&scope.stored, &other.stored),
                    "rollback stored scopes must not overlap"
                );
            }
        }
    }
    Ok(())
}
fn validate_absolute(path: &str) -> Result<()> {
    ensure!(
        path != "/" && super::normalize_container_path(path)? == path,
        "rollback paths must be normalized absolute non-root container paths"
    );
    Ok(())
}
fn overlap(a: &str, b: &str) -> bool {
    super::validate::contained(a, b) || super::validate::contained(b, a)
}

pub(super) fn validate_features(
    profile: &super::Profile,
    version: u32,
    declared: bool,
) -> Result<()> {
    use super::Expectation;
    use crate::model::{Checkpoint, StepName};
    let features = profile.rollback.is_some()
        || profile.steps.keys().any(|s| {
            matches!(
                s,
                StepName::Backup
                    | StepName::AttemptUpgrade
                    | StepName::Rollback
                    | StepName::ExerciseRolledBack
                    | StepName::CleanupBackup
            )
        })
        || profile.checks.iter().any(|c| {
            matches!(
                c.checkpoint,
                Checkpoint::KnownGood
                    | Checkpoint::BackupCreated
                    | Checkpoint::UpgradeFailed
                    | Checkpoint::RolledBack
                    | Checkpoint::BackupRemoved
            ) || matches!(c.expectation, Expectation::Changed { .. })
        });
    ensure!(
        !features || version >= 5,
        "rollback requires recipe schema version 5"
    );
    ensure!(
        !features || declared,
        "rollback features require declared rollback scenario"
    );
    for check in &profile.checks {
        ensure!(
            !declared || !check.id.starts_with("__theseus_rollback_"),
            "reserved rollback check ID"
        );
        ensure!(
            !matches!(check.expectation, Expectation::Changed { .. })
                || check.checkpoint == Checkpoint::UpgradeFailed,
            "changed requires upgrade_failed checkpoint"
        );
    }
    Ok(())
}

pub(super) fn validate_profile(profile: &super::Profile, observe: &[String]) -> Result<()> {
    use super::Expectation;
    use crate::model::{Checkpoint, StepName};
    let policy = profile
        .rollback
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("rollback requires explicit policy"))?;
    validate_rollback_contract(policy)?;
    ensure!(profile.checks.iter().any(|c| c.checkpoint == Checkpoint::BaseInstalled && matches!(&c.expectation, Expectation::Executable { expected_exit: 0, stdout_contains: Some(s), .. } if !s.trim().is_empty())), "rollback requires BaseInstalled Executable marker");
    ensure!(profile.checks.iter().any(|c| c.checkpoint == Checkpoint::KnownGood && matches!(&c.expectation, Expectation::Command { expected_exit: 0, stdout_contains: Some(s), .. } if !s.trim().is_empty())), "rollback requires KnownGood data reader");
    let resolve = |id: &String| -> Result<&super::Check> {
        profile
            .checks
            .iter()
            .find(|c| &c.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown rollback check {id}"))
    };
    for id in &policy.recovery.change_checks {
        let c = resolve(id)?;
        ensure!(
            c.checkpoint == Checkpoint::UpgradeFailed
                && matches!(c.expectation, Expectation::Changed { .. }),
            "rollback change_checks require Changed at UpgradeFailed"
        );
    }
    for id in &policy.recovery.binary_checks {
        let c = resolve(id)?;
        ensure!(
            c.checkpoint == Checkpoint::RolledBack
                && matches!(c.expectation, Expectation::Unchanged { .. }),
            "rollback binary_checks require Unchanged at RolledBack"
        );
    }
    for id in &policy.recovery.runtime_checks {
        let c = resolve(id)?;
        let Expectation::Executable {
            name,
            resolved_path,
            args,
            expected_exit: 0,
            stdout_contains: Some(marker),
            ..
        } = &c.expectation
        else {
            anyhow::bail!(
                "runtime_checks require Executable expected_exit0 with known-good marker"
            );
        };
        ensure!(
            c.checkpoint == Checkpoint::RolledBack && !marker.trim().is_empty(),
            "runtime_checks require RolledBack and nonempty marker"
        );
        ensure!(profile.checks.iter().any(|base| base.checkpoint == Checkpoint::BaseInstalled && matches!(&base.expectation, Expectation::Executable { name: n, resolved_path: p, args: a, expected_exit: 0, stdout_contains: Some(m), .. } if n == name && p == resolved_path && a == args && m == marker)), "rollback runtime must match known-good Executable name/path/args/marker");
        ensure!(
            policy.recovery.binary_checks.iter().any(|id| resolve(id)
                .is_ok_and(|b| b.expectation.path() == Some(resolved_path.as_str()))),
            "rollback runtime path must equal a binary Unchanged path"
        );
    }
    for id in &policy.recovery.data_checks {
        let c = resolve(id)?;
        ensure!(
            c.checkpoint == Checkpoint::RolledBack
                && matches!(&c.expectation, Expectation::Command { expected_exit: 0, stdout_contains: Some(s), .. } if !s.trim().is_empty()),
            "rollback data_checks require Command reader at RolledBack"
        );
    }
    match &policy.recovery.restoration {
        RestorationPolicy::BinaryOnly => {
            ensure!(
                !profile.checks.iter().any(|c| matches!(
                    c.checkpoint,
                    Checkpoint::BackupCreated | Checkpoint::BackupRemoved
                )),
                "binary_only has no backup checkpoints"
            );
            ensure!(
                !profile.steps.contains_key(&StepName::Backup)
                    && !profile.steps.contains_key(&StepName::CleanupBackup),
                "binary_only has no backup steps"
            );
        }
        RestorationPolicy::RestoreData {
            backup_root,
            scopes,
        } => {
            ensure!(
                profile.steps.contains_key(&StepName::Backup)
                    && profile.steps.contains_key(&StepName::CleanupBackup),
                "restore_data requires backup and cleanup_backup"
            );
            for path in std::iter::once(backup_root).chain(scopes.iter().map(|s| &s.source)) {
                ensure!(
                    observe.iter().any(|r| super::validate::contained(path, r)),
                    "rollback scope outside observed roots"
                );
            }
            for id in &policy.recovery.binary_checks {
                let path = resolve(id)?.expectation.path().unwrap();
                for scope in scopes {
                    ensure!(
                        !super::validate::contained(path, &scope.source)
                            && !super::validate::contained(&scope.source, path),
                        "binary and data scopes overlap"
                    );
                }
                ensure!(
                    !super::validate::contained(path, backup_root)
                        && !super::validate::contained(backup_root, path),
                    "binary and backup scopes overlap"
                );
            }
        }
    }
    Ok(())
}
