//! Version-bound finite bindings over raw native client documents.
use super::{Check, Expectation, JsonAssertion, Profile, json::scalar};
use crate::{
    mcp::McpProbe,
    model::{Checkpoint, ScenarioKind},
};
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpDelivery {
    Standalone,
    Plugin,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpClientClaim {
    ConfigurationOnly,
    NativeHealth,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpRegistrationState {
    Active,
    Disabled,
    Removed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpCheckpointContract {
    pub state: McpRegistrationState,
    pub registration: String,
    pub preservation: Vec<String>,
    pub activation: Option<String>,
    pub probe: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpLifecycleContract {
    pub adapter: String,
    pub delivery: McpDelivery,
    pub client_claim: McpClientClaim,
    pub server_id: String,
    pub client_alias: String,
    pub scope: String,
    pub client_version: String,
    pub client_check: String,
    pub client_artifact: String,
    pub checkpoints: BTreeMap<Checkpoint, McpCheckpointContract>,
}
pub(crate) fn checkpoints(s: ScenarioKind) -> Result<Vec<(Checkpoint, McpRegistrationState)>> {
    use Checkpoint::*;
    use McpRegistrationState::*;
    Ok(match s {
        ScenarioKind::Clean => vec![(Configured, Active), (Uninstalled, Removed)],
        ScenarioKind::Upgrade => vec![
            (Configured, Active),
            (TargetInstalled, Active),
            (Uninstalled, Removed),
        ],
        ScenarioKind::PluginLifecycle => vec![
            (BaseInstalled, Active),
            (TargetInstalled, Active),
            (PluginDisabled, Disabled),
            (PluginEnabled, Active),
            (Uninstalled, Removed),
        ],
        _ => bail!("MCP contract requires clean, upgrade or plugin_lifecycle"),
    })
}
fn text(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= 256 && !s.contains('\0')
}
pub(crate) fn validate_contract(p: &McpLifecycleContract, s: ScenarioKind) -> Result<()> {
    for v in [
        &p.adapter,
        &p.server_id,
        &p.client_alias,
        &p.scope,
        &p.client_version,
    ] {
        ensure!(text(v), "MCP identity requires 1..256 bytes");
    }
    ensure!(
        p.server_id != "companion",
        "MCP target cannot use reserved companion identity companion (/bin/true with empty args)"
    );
    super::plugin::validate_client_artifact(&p.client_artifact)?;
    match p.adapter.as_str() {
        "claude-code-2.1.312" => {
            ensure!(
                p.client_version == "2.1.312 (Claude Code)"
                    && p.client_claim == McpClientClaim::NativeHealth,
                "Claude adapter requires exact version and native_health"
            );
            ensure!(
                match p.delivery {
                    McpDelivery::Standalone =>
                        ["user", "project", "local"].contains(&p.scope.as_str()),
                    McpDelivery::Plugin => p.scope == "user",
                },
                "unsupported Claude MCP scope"
            );
        }
        "codex-cli-0.166.0" => ensure!(
            p.client_version == "codex-cli 0.166.0"
                && p.client_claim == McpClientClaim::ConfigurationOnly
                && p.delivery == McpDelivery::Standalone
                && p.scope == "global",
            "Codex supports configuration_only standalone global"
        ),
        _ => bail!("unsupported MCP adapter"),
    }
    ensure!(
        (p.delivery == McpDelivery::Plugin) == (s == ScenarioKind::PluginLifecycle),
        "MCP delivery/scenario mismatch"
    );
    ensure!(
        p.delivery != McpDelivery::Standalone || p.client_alias == p.server_id,
        "standalone alias must equal server identity"
    );
    let expected = checkpoints(s)?;
    ensure!(
        expected.len() == p.checkpoints.len(),
        "wrong MCP checkpoint set"
    );
    let mut ids = BTreeSet::new();
    for id in std::iter::once(&p.client_check).chain(p.checkpoints.values().flat_map(|c| {
        std::iter::once(&c.registration)
            .chain(&c.preservation)
            .chain(c.activation.iter())
            .chain(c.probe.iter())
    })) {
        ensure!(
            text(id) && ids.insert(id),
            "MCP proof IDs require unique 1..256 bytes"
        );
    }
    for (cp, state) in expected {
        let c = p
            .checkpoints
            .get(&cp)
            .ok_or_else(|| anyhow::anyhow!("missing MCP checkpoint {cp}"))?;
        ensure!(
            c.state == state && !c.preservation.is_empty(),
            "wrong MCP state or missing preservation"
        );
        ensure!(
            c.probe.is_some() == (state == McpRegistrationState::Active),
            "typed probe required exactly at active checkpoints"
        );
        ensure!(
            c.activation.is_some() == (p.client_claim == McpClientClaim::NativeHealth),
            "activation contradicts adapter claim"
        );
    }
    Ok(())
}
fn resolve<'a>(checks: &'a [Check], id: &str, cp: Checkpoint) -> Result<&'a Check> {
    let found = checks.iter().filter(|c| c.id == id).collect::<Vec<_>>();
    ensure!(
        found.len() == 1 && found[0].checkpoint == cp,
        "MCP proof {id} must exist once at {cp}"
    );
    Ok(found[0])
}
fn json(check: &Check) -> Result<&[JsonAssertion]> {
    match &check.expectation {
        Expectation::JsonCommand {
            expected_exit: 0,
            assertions,
            ..
        } => Ok(assertions),
        _ => bail!("MCP finite proof requires json_command exit 0"),
    }
}
fn equal(a: &[JsonAssertion], pointer: &str, value: &Value) -> bool {
    a.iter().any(|a|matches!(a,JsonAssertion::Equals{pointer:p,equals_json} if p==pointer&&scalar(equals_json).is_ok_and(|v|v==*value)))
}
fn require(a: &[JsonAssertion], pointer: &str, value: impl Into<Value>) -> Result<()> {
    ensure!(
        equal(a, pointer, &value.into()),
        "missing MCP scalar assertion {pointer}"
    );
    Ok(())
}
fn absent(a: &[JsonAssertion], pointer: &str) -> Result<()> {
    ensure!(
        a.iter()
            .any(|a| matches!(a,JsonAssertion::Absent{pointer:p} if p==pointer)),
        "missing MCP absence assertion {pointer}"
    );
    Ok(())
}
fn literal(a: &[JsonAssertion], pointer: &str) -> Result<String> {
    a.iter()
        .find_map(|a| match a {
            JsonAssertion::Equals {
                pointer: p,
                equals_json,
            } if p == pointer => scalar(equals_json).ok()?.as_str().map(str::to_owned),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("missing MCP string {pointer}"))
}
fn escape(v: &str) -> String {
    v.replace('~', "~0").replace('/', "~1")
}
fn argv(a: &[JsonAssertion], root: &str, argv: &[String]) -> Result<()> {
    ensure!(!argv.is_empty(), "missing registration argv");
    require(a, &format!("{root}/command"), argv[0].clone())?;
    for (i, v) in argv[1..].iter().enumerate() {
        require(a, &format!("{root}/args/{i}"), v.clone())?;
    }
    absent(a, &format!("{root}/args/{}", argv.len() - 1))
}
fn native_argv(a: &[JsonAssertion], root: &str) -> Result<Vec<String>> {
    let mut args = vec![literal(a, &format!("{root}/command"))?];
    for i in 0..32 {
        let key = format!("{root}/args/{i}");
        if absent(a, &key).is_ok() {
            return Ok(args);
        }
        args.push(literal(a, &key)?);
    }
    bail!("MCP argv exceeds finite contract bound")
}
fn array(
    a: &[JsonAssertion],
    pointer: &str,
    key: &str,
    id: &str,
    count: u32,
    values: &[(&str, Value)],
) -> Result<()> {
    let found = a
        .iter()
        .filter_map(|a| {
            if let JsonAssertion::ArrayMatch {
                pointer: p,
                identity,
                count: c,
                values: v,
            } = a
            {
                (p == pointer
                    && identity
                        .iter()
                        .any(|p| p.pointer == key && scalar(&p.equals_json).is_ok_and(|v| v == id)))
                .then_some((identity, c, v))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    ensure!(
        found.len() == 1,
        "MCP array requires one identity-only match"
    );
    let (identity, c, v) = found[0];
    ensure!(
        identity.len() == 1 && *c == count,
        "MCP identity must be matched alone and cardinality exact"
    );
    for (pointer, value) in values {
        ensure!(v.iter().any(|p|p.pointer==*pointer&&scalar(&p.equals_json).is_ok_and(|v|v==*value)),"missing MCP array value {pointer}");
    }
    Ok(())
}
fn claude_maps() -> [&'static str; 3] {
    [
        "/client/mcpServers",
        "/project/mcpServers",
        "/client/projects/~1workspace/mcpServers",
    ]
}
fn selected(p: &McpLifecycleContract) -> usize {
    match p.scope.as_str() {
        "project" => 1,
        "local" => 2,
        _ => 0,
    }
}
fn registration(
    p: &McpLifecycleContract,
    cp: Checkpoint,
    a: &[JsonAssertion],
    probe: Option<&McpProbe>,
    last: &[String],
) -> Result<Vec<String>> {
    let state = p.checkpoints[&cp].state;
    let removed = state == McpRegistrationState::Removed;
    let id = escape(&p.server_id);
    if p.adapter.starts_with("codex") {
        array(
            a,
            "/list",
            "/name",
            &p.server_id,
            if removed { 0 } else { 1 },
            &if removed {
                vec![]
            } else {
                vec![("/enabled", true.into())]
            },
        )?;
        if removed {
            return Ok(vec![]);
        }
        require(a, "/get/name", p.server_id.clone())?;
        require(a, "/get/enabled", true)?;
        require(a, "/get/transport/type", "stdio")?;
        let args = probe
            .ok_or_else(|| anyhow::anyhow!("active probe missing"))?
            .argv
            .clone();
        argv(a, "/get/transport", &args)?;
        return Ok(args);
    }
    for (i, map) in claude_maps().iter().enumerate() {
        if p.delivery == McpDelivery::Plugin || removed || i != selected(p) {
            absent(a, &format!("{map}/{id}"))?;
        }
    }
    if p.delivery == McpDelivery::Standalone {
        if removed {
            return Ok(vec![]);
        }
        let args = probe
            .ok_or_else(|| anyhow::anyhow!("active probe missing"))?
            .argv
            .clone();
        argv(a, &format!("{}/{id}", claude_maps()[selected(p)]), &args)?;
        return Ok(args);
    }
    // Plugin root remains raw native array. Bind an asserted array index back to
    // the id-only cardinality check before using indexed scalar/absence fields.
    let plugin = p
        .client_alias
        .strip_prefix("plugin:")
        .and_then(|v| v.strip_suffix(&format!(":{}", p.server_id)))
        .ok_or_else(|| anyhow::anyhow!("invalid plugin alias"))?;
    let inventory_id = a
        .iter()
        .find_map(|a| match a {
            JsonAssertion::ArrayMatch {
                pointer, identity, ..
            } if pointer == "/plugins" => identity.iter().find_map(|v| {
                let id = scalar(&v.equals_json).ok()?.as_str()?.to_owned();
                (v.pointer == "/id"
                    && id
                        .strip_prefix(&format!("{plugin}@"))
                        .is_some_and(|suffix| !suffix.is_empty()))
                .then_some(id)
            }),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("plugin inventory identity must match client alias"))?;
    array(
        a,
        "/plugins",
        "/id",
        &inventory_id,
        if removed { 0 } else { 1 },
        &if removed {
            vec![]
        } else {
            vec![
                ("/scope", p.scope.clone().into()),
                ("/enabled", (state != McpRegistrationState::Disabled).into()),
            ]
        },
    )?;
    if removed {
        return Ok(vec![]);
    }
    let indices = a
        .iter()
        .filter_map(|v| {
            if let JsonAssertion::Equals {
                pointer,
                equals_json,
            } = v
            {
                pointer
                    .strip_prefix("/plugins/")?
                    .strip_suffix("/id")
                    .filter(|i| i.parse::<usize>().is_ok())
                    .filter(|_| scalar(equals_json).is_ok_and(|v| v == inventory_id))
                    .map(str::to_owned)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    ensure!(
        indices.len() == 1,
        "plugin index requires one unique target identity guard"
    );
    let index = &indices[0];
    let root = format!("/plugins/{index}");
    let source = literal(a, &format!("{root}/readFromFolder"))?;
    ensure!(
        source.starts_with('/') && !source.split('/').any(|x| x == ".."),
        "unsafe plugin source"
    );
    let args = native_argv(a, &format!("{root}/mcpServers/{id}"))?;
    let resolved = args
        .iter()
        .map(|s| s.replace("${CLAUDE_PLUGIN_ROOT}", &source))
        .collect::<Vec<_>>();
    ensure!(
        !resolved.iter().any(|s| s.contains("${")),
        "unresolved plugin root placeholder"
    );
    ensure!(
        resolved == probe.map(|p| p.argv.as_slice()).unwrap_or(last),
        "plugin source/argv differs from independent probe"
    );
    Ok(resolved)
}
fn preservation(p: &McpLifecycleContract, a: &[JsonAssertion]) -> Result<()> {
    // Version-bound examples preserve a distinct MCP companion and a documented
    // supported client setting. Values are explicit recipe assertions.
    if p.adapter.starts_with("codex") {
        require(a, "/companion/name", "companion")?;
        require(a, "/companion/enabled", true)?;
        require(a, "/companion/transport/type", "stdio")?;
        argv(a, "/companion/transport", &["/bin/true".into()])?;
        require(a, "/settings_preserved", true)
    } else {
        let maps = claude_maps();
        let root = format!("{}/companion", maps[0]);
        argv(a, &root, &["/bin/true".into()])?;
        absent(a, &format!("{}/companion", maps[1]))?;
        absent(a, &format!("{}/companion", maps[2]))?;
        require(a, "/client/hasCompletedOnboarding", true)
    }
}
pub(crate) fn bindings(p: &McpLifecycleContract, s: ScenarioKind, checks: &[Check]) -> Result<()> {
    validate_contract(p, s)?;
    let cps = checkpoints(s)?;
    let first = cps[0].0;
    require(
        json(resolve(checks, &p.client_check, first)?)?,
        "/version",
        p.client_version.clone(),
    )?;
    let mut last = vec![];
    for (cp, _) in cps {
        let c = &p.checkpoints[&cp];
        let probe = if let Some(id) = &c.probe {
            let Expectation::McpStdio { probe } = &resolve(checks, id, cp)?.expectation else {
                bail!("active proof must be typed mcp_stdio")
            };
            probe.validate()?;
            Some(probe)
        } else {
            None
        };
        let a = json(resolve(checks, &c.registration, cp)?)?;
        let launch = registration(p, cp, a, probe, &last)?;
        if !launch.is_empty() {
            last = launch;
        }
        let preserved = c
            .preservation
            .iter()
            .map(|id| json(resolve(checks, id, cp)?).map(<[_]>::to_vec))
            .collect::<Result<Vec<_>>>()?
            .concat();
        preservation(p, &preserved).map_err(|e| anyhow::anyhow!(
            "MCP preservation requires reserved companion=/bin/true with empty args, Claude user hasCompletedOnboarding=true or Codex settings_preserved=true for the supported exact prefix: {e}"
        ))?;
        if let Some(id) = &c.activation {
            let check = resolve(checks, id, cp)?;
            if let Expectation::Command {
                expected_exit: 0,
                stdout_contains: Some(marker),
                ..
            } = &check.expectation
            {
                ensure!(
                    c.state == McpRegistrationState::Active
                        && *marker
                            == format!("{}: {} - ✔ Connected", p.client_alias, last.join(" ")),
                    "native health marker must bind exact alias/source/Connected at active checkpoint"
                );
                continue;
            }
            let a = json(check)?;
            require(a, "/status", 0)?;
            require(a, "/alias", p.client_alias.clone())?;
            require(a, "/connected", c.state == McpRegistrationState::Active)?;
            require(
                a,
                "/source",
                if c.state == McpRegistrationState::Active {
                    last.join(" ")
                } else {
                    String::new()
                },
            )?;
        }
    }
    let bound = p
        .checkpoints
        .values()
        .filter_map(|c| c.probe.as_ref())
        .collect::<BTreeSet<_>>();
    ensure!(
        checks
            .iter()
            .filter(|c| matches!(c.expectation, Expectation::McpStdio { .. }))
            .all(|c| bound.contains(&c.id)),
        "unbound MCP probe outside active checkpoint"
    );
    Ok(())
}
pub fn validate_mcp_contract(profile: &Profile, scenario: ScenarioKind) -> Result<()> {
    let Some(p) = &profile.mcp else { return Ok(()) };
    bindings(p, scenario, &profile.checks)?;
    for step in profile.steps.values() {
        ensure!(step.pass_env.is_empty(), "MCP forbids pass_env");
    }
    for c in &profile.checks {
        if let Expectation::Command { pass_env, .. }
        | Expectation::JsonCommand { pass_env, .. }
        | Expectation::Executable { pass_env, .. } = &c.expectation
        {
            ensure!(pass_env.is_empty(), "MCP forbids pass_env");
        }
    }
    if p.delivery == McpDelivery::Plugin {
        let plugin = profile
            .plugin
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("plugin MCP requires PluginContract"))?;
        ensure!(
            plugin
                .id
                .split_once('@')
                .is_some_and(|(name, market)| !name.is_empty()
                    && !market.is_empty()
                    && p.client_alias == format!("plugin:{name}:{}", p.server_id)),
            "MCP alias must name PluginContract identity"
        );
        ensure!(
            plugin.scope.as_str() == p.scope
                && plugin.client_artifact == p.client_artifact
                && plugin.client_version == p.client_version,
            "incompatible plugin MCP contract"
        );
        for (cp, c) in &p.checkpoints {
            let a = json(resolve(&profile.checks, &c.registration, *cp)?)?;
            array(
                a,
                "/plugins",
                "/id",
                &plugin.id,
                if c.state == McpRegistrationState::Removed {
                    0
                } else {
                    1
                },
                &[],
            )?;
        }
        let plugin_ids = std::iter::once(&plugin.client_check)
            .chain(plugin.checkpoints.values().flat_map(|c| {
                std::iter::once(&c.inventory)
                    .chain(&c.preservation)
                    .chain(&c.components)
            }))
            .collect::<BTreeSet<_>>();
        ensure!(
            std::iter::once(&p.client_check)
                .chain(p.checkpoints.values().flat_map(|c| {
                    std::iter::once(&c.registration)
                        .chain(&c.preservation)
                        .chain(c.activation.iter())
                        .chain(c.probe.iter())
                }))
                .all(|id| !plugin_ids.contains(id)),
            "MCP and plugin roles must be distinct"
        );
    }
    Ok(())
}
