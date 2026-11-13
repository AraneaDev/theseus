use super::super::*;
use crate::model::ScenarioKind;
use crate::native::model::*;
use anyhow::{Result, ensure};
use std::collections::BTreeSet;

fn commitments(core: &str, integration: &str, full: &str) -> Result<()> {
    use theseus_native_protocol::v2;
    ensure!(
        v2::integration_manifest_digest(v2::Digest::parse(core)?, v2::Digest::parse(integration)?)
            .to_hex()
            == full,
        "integration manifest composition mismatch"
    );
    Ok(())
}
pub(super) fn common_validate(
    envelope: (&str, u32),
    expected: &str,
    profile: &str,
    run: &str,
    digests: [&str; 6],
    requirements: &IntegrationRequirements,
    selected: &[ScenarioKind],
) -> Result<()> {
    let (kind, schema) = envelope;
    ensure!(
        kind == expected && schema == 2 && profile == crate::native::integration::PROFILE,
        "unsupported integration envelope"
    );
    text(run, 128)?;
    for d in digests {
        digest(d)?;
    }
    commitments(digests[2], digests[3], digests[4])?;
    ensure!(
        requirements.digest()? == digests[5],
        "compiled integration requirements changed"
    );
    ensure!(
        !selected.is_empty() && selected.len() <= 3,
        "invalid integration selection"
    );
    let mut seen = BTreeSet::new();
    for scenario in selected {
        ordinary(*scenario)?;
        ensure!(seen.insert(scenario), "duplicate integration selection");
    }
    requirements.validate(selected)
}
