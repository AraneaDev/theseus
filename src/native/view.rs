//! Bounded native views preserve status/provenance and independent cleanup states.
use super::model::*;
use anyhow::{Result, ensure};
use serde::Serialize;

#[derive(Debug, Clone, Copy)]
pub enum Selector {
    Overview,
    NativeFiles,
    Commands,
    Dispositions,
    Identities,
    Failures,
}
#[derive(Debug, Serialize)]
pub struct Page {
    pub records: Vec<serde_json::Value>,
    pub offset: usize,
    pub next_offset: Option<usize>,
}
pub fn evidence_page(
    report: &NativeReport,
    selector: Selector,
    offset: usize,
    limit: usize,
) -> Result<Page> {
    super::storage::validate_report(report)?;
    ensure!(
        (1..=64).contains(&limit),
        "native page limit must be 1..=64"
    );
    let mut records = Vec::new();
    let mut total = 0usize;
    let mut bytes = 0usize;
    let mut full = false;
    let mut push = |value: serde_json::Value| {
        if total >= offset && records.len() < limit && !full {
            let length = serde_json::to_vec(&value).map_or(usize::MAX, |b| b.len());
            if bytes.saturating_add(length) > 65536 {
                full = true;
            } else {
                bytes += length;
                records.push(value);
            }
        }
        total += 1;
    };
    match selector {
        Selector::Overview => push(
            serde_json::json!({"kind":report.kind,"schema":report.schema,"run_id":report.run_id,"outcome":report.outcome,"recipe_digest":report.recipe_digest,"failure_count":report.failures.len()}),
        ),
        Selector::Failures => {
            for failure in &report.failures {
                push(serde_json::json!({"scope":"run","failure":failure}));
            }
            for scenario in &report.scenarios {
                for stage in &scenario.stages {
                    for failure in &stage.failures {
                        push(
                            serde_json::json!({"scenario_id":scenario.id,"stage":stage.name,"failure":failure}),
                        );
                    }
                }
            }
        }
        Selector::Identities => {
            for identity in &report.identities {
                push(serde_json::to_value(identity)?);
            }
            for claim in &report.provider_claims {
                push(serde_json::json!({"provenance":"provider_reported","claim":claim}));
            }
        }
        selector => {
            for scenario in &report.scenarios {
                if matches!(selector, Selector::Dispositions) {
                    push(
                        serde_json::json!({"scenario_id":scenario.id,"ownership_id":scenario.ownership_id,"dispositions":scenario.dispositions}),
                    );
                } else {
                    for stage in &scenario.stages {
                        if matches!(selector, Selector::NativeFiles) {
                            for file in &stage.native_files {
                                push(
                                    serde_json::json!({"scenario_id":scenario.id,"stage":stage.name,"native_file":file}),
                                );
                            }
                        } else {
                            for command in &stage.commands {
                                push(
                                    serde_json::json!({"scenario_id":scenario.id,"stage":stage.name,"command":command}),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    ensure!(offset <= total, "native page offset exceeds record count");
    ensure!(
        offset == total || !records.is_empty(),
        "native record exceeds page byte budget"
    );
    let end = offset + records.len();
    Ok(Page {
        records,
        offset,
        next_offset: (end < total).then_some(end),
    })
}
pub fn preview(
    recipe: &super::recipe::ValidatedNativeRecipe,
    launch: Option<&super::launch::LaunchAuthority>,
) -> Result<String> {
    recipe.recipe.validate()?;
    let target = launch
        .map(|l| {
            format!(
                "{} ({}, {})",
                l.view().target_id,
                l.view().profile,
                l.view().fingerprint
            )
        })
        .unwrap_or_else(|| {
            "unsupplied; execution requires explicit native launch authority".into()
        });
    let mut text = format!(
        "Native catalog preview\nTarget: {target}\nRecipe: {}\nManifest: {}\nOS: {:?}\nRole: current helper account\nScenarios: {:?}\nLeaves: {:?}\nBase reader: length={} sha256={}\nTarget reader: length={} sha256={}\nLimits: {:?}\n",
        recipe.digest,
        recipe.recipe.manifest_digest,
        recipe.recipe.os,
        recipe.recipe.scenarios,
        recipe.recipe.leaves,
        recipe.recipe.base.length,
        recipe.recipe.base.sha256,
        recipe.recipe.target.length,
        recipe.recipe.target.sha256,
        recipe.recipe.limits,
    );
    let describe = |command: &CatalogCommand| {
        let literal = command
            .stdout_literal
            .as_ref()
            .map(|s| {
                let mut end = s.len().min(128);
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                format!(
                    "{:?}{}",
                    &s[..end],
                    if end < s.len() {
                        " [preview truncated]"
                    } else {
                        ""
                    }
                )
            })
            .unwrap_or_else(|| "none".into());
        format!(
            "{}: {} expected={:?} stdout_literal={}\n",
            command.id,
            command.program.as_str(),
            command.expected_status,
            literal
        )
    };
    for (step, command) in &recipe.recipe.steps {
        text.push_str(&format!("Step {step}: {}", describe(command)));
    }
    for check in recipe.recipe.checks.iter().take(32) {
        text.push_str(&format!(
            "Check {:?}: {}",
            check.checkpoint,
            describe(&check.command)
        ));
    }
    if recipe.recipe.checks.len() > 32 {
        text.push_str(&format!("Check preview truncated: showing 32 of {}; use recipe inspection pages for all checks.\n",recipe.recipe.checks.len()));
    }
    Ok(text)
}
