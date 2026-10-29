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
