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
pub struct RollbackContract {
    pub failure: ExpectedFailureContract,
    pub change_checks: Vec<String>,
    pub binary_checks: Vec<String>,
    pub data_checks: Vec<String>,
    pub restoration: RestorationPolicy,
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
    let mut references = BTreeSet::new();
    for ids in [
        &contract.change_checks,
        &contract.binary_checks,
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
