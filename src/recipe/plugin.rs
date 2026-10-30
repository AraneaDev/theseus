//! Declared, bounded plugin lifecycle proof bindings.
use super::{Expectation, JsonAssertion, Profile, json::scalar};
use crate::model::{Checkpoint, StepName};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginScope {
    User,
    Project,
    Local,
}
impl PluginScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Local => "local",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCheckpointContract {
    pub inventory: String,
    pub preservation: Vec<String>,
    pub components: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginContract {
    pub id: String,
    pub scope: PluginScope,
    pub base_version: String,
    pub target_version: String,
    pub client_version: String,
    pub client_check: String,
    pub client_artifact: String,
    pub checkpoints: BTreeMap<Checkpoint, PluginCheckpointContract>,
}
pub(crate) const CHECKPOINTS: [Checkpoint; 5] = [
    Checkpoint::BaseInstalled,
    Checkpoint::TargetInstalled,
    Checkpoint::PluginDisabled,
    Checkpoint::PluginEnabled,
    Checkpoint::Uninstalled,
];
pub(crate) const STEPS: [StepName; 5] = [
    StepName::InstallBase,
    StepName::Upgrade,
    StepName::DisablePlugin,
    StepName::EnablePlugin,
    StepName::Uninstall,
];
pub(crate) fn validate_contract(p: &PluginContract) -> Result<()> {
    for s in [&p.id, &p.base_version, &p.target_version, &p.client_version] {
        ensure!(
            !s.trim().is_empty() && s.len() <= 256 && !s.contains('\0'),
            "plugin identity/version requires 1..256 bytes"
        );
    }
    ensure!(
        p.base_version != p.target_version,
        "plugin versions must be distinct"
    );
    ensure!(
        !p.client_artifact.is_empty()
            && p.client_artifact
                .split('/')
                .all(|x| !x.is_empty() && x != "." && x != ".." && !x.contains('\0'))
            && !p.client_artifact.contains('\\'),
        "unsafe client_artifact path"
    );
    ensure!(
        p.checkpoints.len() == 5 && CHECKPOINTS.iter().all(|cp| p.checkpoints.contains_key(cp)),
        "plugin requires exactly five checkpoint entries"
    );
    let mut ids = BTreeSet::new();
    for id in std::iter::once(&p.client_check).chain(p.checkpoints.values().flat_map(|c| {
        std::iter::once(&c.inventory)
            .chain(&c.preservation)
            .chain(&c.components)
    })) {
        ensure!(
            !id.trim().is_empty() && ids.insert(id),
            "plugin requires nonempty unique proof ID: {id}"
        );
    }
    for (cp, c) in &p.checkpoints {
        ensure!(
            !c.preservation.is_empty(),
            "{cp} requires preservation proof"
        );
        ensure!(
            !matches!(cp, Checkpoint::BaseInstalled | Checkpoint::TargetInstalled)
                || !c.components.is_empty(),
            "{cp} requires component proof"
        );
        ensure!(
            matches!(cp, Checkpoint::BaseInstalled | Checkpoint::TargetInstalled)
                || c.components.is_empty(),
            "component proof only belongs at base/target"
        );
    }
    Ok(())
}
pub(crate) fn inventory_assertions(
    p: &PluginContract,
    cp: Checkpoint,
    assertions: &[JsonAssertion],
) -> Result<()> {
    let id = Value::String(p.id.clone());
    let roots = assertions
        .iter()
        .filter(|a| {
            matches!(a, JsonAssertion::ArrayMatch { pointer, identity, .. }
            if pointer.is_empty() && identity.iter().any(|v|
                v.pointer == "/id" && scalar(&v.equals_json).is_ok_and(|value| value == id)))
        })
        .collect::<Vec<_>>();
    ensure!(roots.len() == 1, "inventory requires one root ArrayMatch");
    let JsonAssertion::ArrayMatch {
        identity,
        count,
        values,
        ..
    } = roots[0]
    else {
        unreachable!()
    };
    ensure!(
        identity.len() == 1
            && identity[0].pointer == "/id"
            && scalar(&identity[0].equals_json).is_ok_and(|value| value == id),
        "inventory identity must match id alone"
    );
    ensure!(
        *count == if cp == Checkpoint::Uninstalled { 0 } else { 1 },
        "inventory count must match lifecycle checkpoint"
    );
    if cp != Checkpoint::Uninstalled {
        for (pointer, value) in [
            ("/scope", Value::String(p.scope.as_str().into())),
            (
                "/version",
                Value::String(if cp == Checkpoint::BaseInstalled {
                    p.base_version.clone()
                } else {
                    p.target_version.clone()
                }),
            ),
            ("/enabled", Value::Bool(cp != Checkpoint::PluginDisabled)),
        ] {
            ensure!(
                values
                    .iter()
                    .any(|v| v.pointer == pointer
                        && scalar(&v.equals_json).is_ok_and(|v| v == value)),
                "inventory values must bind scope/version/enabled at {cp}"
            );
        }
    }
    Ok(())
}
pub fn validate_plugin_contract(profile: &Profile) -> Result<()> {
    let p = profile
        .plugin
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("plugin_lifecycle requires plugin contract"))?;
    validate_contract(p)?;
    for step in STEPS {
        ensure!(
            profile.steps.contains_key(&step),
            "plugin_lifecycle requires step {step}"
        );
    }
    for step in profile.steps.values() {
        ensure!(
            step.pass_env.is_empty(),
            "plugin_lifecycle forbids pass_env"
        );
    }
    for c in &profile.checks {
        if let Expectation::Command { pass_env, .. }
        | Expectation::JsonCommand { pass_env, .. }
        | Expectation::Executable { pass_env, .. } = &c.expectation
        {
            ensure!(pass_env.is_empty(), "plugin_lifecycle forbids pass_env");
        }
    }
    let resolve = |id: &str, cp| -> Result<&super::Check> {
        let found = profile
            .checks
            .iter()
            .filter(|c| c.id == id)
            .collect::<Vec<_>>();
        ensure!(found.len() == 1, "plugin proof {id} must exist once");
        ensure!(
            found[0].checkpoint == cp,
            "plugin proof {id} requires checkpoint {cp}"
        );
        Ok(found[0])
    };
    let client = resolve(&p.client_check, Checkpoint::BaseInstalled)?;
    ensure!(
        matches!(&client.expectation,Expectation::JsonCommand{expected_exit:0,assertions,..} if assertions.iter().any(|a|matches!(a,JsonAssertion::Equals{pointer,equals_json} if pointer=="/version" && scalar(equals_json).is_ok_and(|v| v == Value::String(p.client_version.clone()))))),
        "client check requires JSON version assertion"
    );
    for (cp, c) in &p.checkpoints {
        let inventory = resolve(&c.inventory, *cp)?;
        let Expectation::JsonCommand {
            expected_exit: 0,
            assertions,
            ..
        } = &inventory.expectation
        else {
            anyhow::bail!("inventory requires json_command with expected_exit 0")
        };
        inventory_assertions(p, *cp, assertions)?;
        for id in &c.preservation {
            ensure!(
                matches!(&resolve(id,*cp)?.expectation,Expectation::JsonCommand{expected_exit:0,assertions,..} if assertions.iter().any(|a|matches!(a,JsonAssertion::Equals{..}|JsonAssertion::ArrayMatch{count:1,..}))),
                "preservation proof requires nonempty scalar assertions"
            );
        }
        for id in &c.components {
            ensure!(
                matches!(&resolve(id,*cp)?.expectation,Expectation::Command{expected_exit:0,stdout_contains:Some(s),..} if !s.trim().is_empty())
                    || matches!(
                        &resolve(id, *cp)?.expectation,
                        Expectation::JsonCommand {
                            expected_exit: 0,
                            ..
                        }
                    ),
                "component proof requires successful command with literal marker or json_command"
            );
        }
    }
    Ok(())
}
