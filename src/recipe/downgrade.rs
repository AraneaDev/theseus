use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The author's declared result of an explicit downgrade attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "expect", rename_all = "snake_case", deny_unknown_fields)]
pub enum DowngradeContract {
    Compatible {
        data_checks: Vec<String>,
    },
    SafeRejection {
        expected_exit: i32,
        stdout_contains: String,
        binary_checks: Vec<String>,
        data_checks: Vec<String>,
    },
}

/// Validate the policy itself. Resolving references to checkpoint checks requires
/// recipe/scenario context and is deliberately separate from this pure validator.
pub fn validate_downgrade_contract(contract: &DowngradeContract) -> Result<()> {
    match contract {
        DowngradeContract::Compatible { data_checks } => {
            validate_references(data_checks, "data_checks")?;
        }
        DowngradeContract::SafeRejection {
            expected_exit,
            stdout_contains,
            binary_checks,
            data_checks,
        } => {
            ensure!(
                (1..=125).contains(expected_exit),
                "safe rejection expected_exit must be in 1..=125"
            );
            ensure!(
                !stdout_contains.trim().is_empty(),
                "safe rejection requires nonempty stdout_contains"
            );
            let binary = validate_references(binary_checks, "binary_checks")?;
            let data = validate_references(data_checks, "data_checks")?;
            ensure!(
                binary.is_disjoint(&data),
                "binary_checks and data_checks must not share reference IDs"
            );
        }
    }
    Ok(())
}

fn validate_references<'a>(ids: &'a [String], role: &str) -> Result<BTreeSet<&'a str>> {
    ensure!(!ids.is_empty(), "{role} requires at least one reference ID");
    let mut unique = BTreeSet::new();
    for id in ids {
        ensure!(
            !id.trim().is_empty(),
            "{role} reference IDs must be nonempty"
        );
        ensure!(
            unique.insert(id.as_str()),
            "{role} reference IDs must be unique"
        );
    }
    Ok(unique)
}

pub(super) fn validate_profile(profile: &super::Profile) -> Result<()> {
    use super::Expectation;
    use crate::model::Checkpoint;
    let policy = profile
        .downgrade
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("downgrade requires an explicit policy"))?;
    validate_downgrade_contract(policy)?;
    let runtime = |checkpoint| {
        profile.checks.iter().any(|check| check.checkpoint == checkpoint && matches!(&check.expectation,
        Expectation::Executable { expected_exit: 0, stdout_contains: Some(marker), .. } if !marker.trim().is_empty()))
    };
    ensure!(
        runtime(Checkpoint::TargetInstalled),
        "downgrade requires a newer Executable contract at target_installed"
    );
    let (checkpoint, data) = match policy {
        DowngradeContract::Compatible { data_checks } => (Checkpoint::Downgraded, data_checks),
        DowngradeContract::SafeRejection { data_checks, .. } => {
            (Checkpoint::DowngradeRejected, data_checks)
        }
    };
    ensure!(
        runtime(checkpoint),
        "downgrade requires an Executable contract at {checkpoint}"
    );
    let resolve = |id: &String| -> Result<&super::Check> {
        let check = profile
            .checks
            .iter()
            .find(|check| &check.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown downgrade check {id}"))?;
        ensure!(
            check.checkpoint == checkpoint,
            "downgrade check {id} requires checkpoint {checkpoint}"
        );
        Ok(check)
    };
    for id in data {
        let check = resolve(id)?;
        match policy {
            DowngradeContract::Compatible { .. } => ensure!(
                matches!(&check.expectation, Expectation::Command { expected_exit: 0, stdout_contains: Some(marker), .. } if !marker.trim().is_empty()),
                "compatible data check {id} requires Command with expected_exit 0 and nonempty stdout_contains"
            ),
            DowngradeContract::SafeRejection { .. } => ensure!(
                matches!(check.expectation, Expectation::Unchanged { .. }),
                "safe rejection data check {id} requires Unchanged"
            ),
        }
    }
    if let DowngradeContract::SafeRejection { binary_checks, .. } = policy {
        for id in binary_checks {
            let check = resolve(id)?;
            ensure!(
                matches!(check.expectation, Expectation::Unchanged { .. }),
                "safe rejection binary check {id} requires Unchanged"
            );
            let binary_path = check.expectation.path().unwrap();
            for data_id in data {
                let data_path = resolve(data_id)?.expectation.path().unwrap();
                ensure!(
                    !super::validate::contained(binary_path, data_path)
                        && !super::validate::contained(data_path, binary_path),
                    "binary and data scopes overlap"
                );
            }
        }
    }
    Ok(())
}
