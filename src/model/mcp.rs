//! Retained binding consistency, not authenticated native-output verification.
use super::rollback_validation::{Invocation, valid_invocation};
use super::*;
use crate::recipe::{self, Check, Expectation};
use std::collections::{BTreeMap, BTreeSet};
fn complete(s: &ScenarioReport) -> bool {
    let sequence = match s.scenario {
        ScenarioKind::Clean => vec![
            "baseline",
            "install_target",
            "target_installed",
            "target_installed:after_checks",
            "configure",
            "configured",
            "configured:after_checks",
            "exercise",
            "uninstall",
            "uninstalled",
            "uninstalled:after_checks",
            "final",
        ],
        ScenarioKind::Upgrade => vec![
            "baseline",
            "install_base",
            "base_installed",
            "base_installed:after_checks",
            "configure",
            "configured",
            "configured:after_checks",
            "upgrade",
            "target_installed",
            "target_installed:after_checks",
            "exercise",
            "uninstall",
            "uninstalled",
            "uninstalled:after_checks",
            "final",
        ],
        ScenarioKind::PluginLifecycle => vec![
            "baseline",
            "install_base",
            "base_installed",
            "base_installed:after_checks",
            "upgrade",
            "target_installed",
            "target_installed:after_checks",
            "disable_plugin",
            "plugin_disabled",
            "plugin_disabled:after_checks",
            "enable_plugin",
            "plugin_enabled",
            "plugin_enabled:after_checks",
            "uninstall",
            "uninstalled",
            "uninstalled:after_checks",
            "final",
        ],
        _ => return false,
    };
    if s.stages.iter().map(|st| st.name.as_str()).ne(sequence) {
        return false;
    }
    let p = s.mcp.as_ref().unwrap();
    let roles = std::iter::once(&p.client_check)
        .chain(p.checkpoints.values().flat_map(|c| {
            std::iter::once(&c.registration)
                .chain(&c.preservation)
                .chain(c.activation.iter())
                .chain(c.probe.iter())
        }))
        .collect::<BTreeSet<_>>();
    let mut ids = BTreeSet::new();
    let mut checks = vec![];
    for stage in &s.stages {
        if stage.snapshots.is_empty() || !stage.valid_json_checks() || !stage.valid_mcp_checks() {
            return false;
        }
        for cmd in &stage.commands {
            if !ids.insert(&cmd.id)
                || (cmd.identity.uid != 10001
                    || cmd.identity.gid != 10001
                    || cmd.identity.home != "/home/theseus")
                || !cmd.forwarded_environment.is_empty()
            {
                return false;
            }
        }
        for c in &stage.checks {
            if !roles.contains(&c.id) {
                if c.mcp_required || c.mcp.is_some() {
                    return false;
                }
                continue;
            }
            if c.outcome != CheckOutcome::Passed
                || stage.checkpoint != Some(c.checkpoint)
                || stage.name != c.checkpoint.to_string()
                || c.command_ids.len() != 1
            {
                return false;
            }
            let Some(cmd) = stage.commands.iter().find(|cmd| cmd.id == c.command_ids[0]) else {
                return false;
            };
            if cmd.phase != format!("check:{}", c.id)
                || cmd.outcome != CommandOutcome::Succeeded
                || cmd.exit_status != Some(0)
                || cmd.timed_out
                || cmd.cancelled
                || cmd.error.is_some()
            {
                return false;
            }
            let expectation = if let Some(proof) = &c.mcp {
                if !c.mcp_required || !proof.verified_pass() {
                    return false;
                }
                Expectation::McpStdio {
                    probe: proof.probe.clone(),
                }
            } else if let Some(proof) = &c.json_expectation {
                if !c.json_required
                    || !proof.verified_pass()
                    || !valid_invocation(&cmd.argv, Invocation::ShellScript)
                {
                    return false;
                }
                Expectation::JsonCommand {
                    script: cmd.argv[2].clone(),
                    timeout_seconds: 120,
                    env: cmd.environment.clone(),
                    pass_env: vec![],
                    expected_exit: proof.expected_exit,
                    max_stdout_bytes: proof.max_stdout_bytes,
                    assertions: proof.assertions.clone(),
                }
            } else if let Some(proof) = &c.command_expectation {
                if !super::rollback_validation::command_role_evidence(stage, c)
                    || proof.executable.is_some()
                {
                    return false;
                }
                Expectation::Command {
                    script: cmd.argv[2].clone(),
                    timeout_seconds: 120,
                    env: cmd.environment.clone(),
                    pass_env: vec![],
                    expected_exit: proof.expected_exit,
                    stdout_contains: Some(proof.stdout_contains.clone()),
                }
            } else {
                continue;
            };
            checks.push(Check {
                id: c.id.clone(),
                checkpoint: c.checkpoint,
                expectation,
            });
        }
    }
    let steps = match s.scenario {
        ScenarioKind::Clean => vec![
            StepName::InstallTarget,
            StepName::Configure,
            StepName::Exercise,
            StepName::Uninstall,
        ],
        ScenarioKind::Upgrade => vec![
            StepName::InstallBase,
            StepName::Configure,
            StepName::Upgrade,
            StepName::Exercise,
            StepName::Uninstall,
        ],
        ScenarioKind::PluginLifecycle => vec![
            StepName::InstallBase,
            StepName::Upgrade,
            StepName::DisablePlugin,
            StepName::EnablePlugin,
            StepName::Uninstall,
        ],
        _ => return false,
    };
    if s.stages.iter().filter_map(|st| st.step).ne(steps) {
        return false;
    }
    for st in s.stages.iter().filter(|st| st.step.is_some()) {
        if st.commands.len() != 1
            || st.checkpoint.is_some()
            || st.name != st.step.unwrap().to_string()
        {
            return false;
        }
        let cmd = &st.commands[0];
        if cmd.phase != st.name
            || !valid_invocation(&cmd.argv, Invocation::ShellScript)
            || cmd.outcome != CommandOutcome::Succeeded
            || cmd.exit_status != Some(0)
        {
            return false;
        }
    }
    if s.stages.first().is_none_or(|st| st.name != "baseline")
        || s.stages.last().is_none_or(|st| st.name != "final")
    {
        return false;
    }
    let profile = recipe::Profile {
        mcp: s.mcp.clone(),
        plugin: s.plugin.clone(),
        rollback: None,
        interrupted_rollback: None,
        downgrade: None,
        steps: BTreeMap::new(),
        checks,
    };
    recipe::validate_mcp_contract(&profile, s.scenario).is_ok()
}
pub fn scenario_outcome(s: &ScenarioReport) -> RunOutcome {
    let Some(p) = &s.mcp else {
        return RunOutcome::Passed;
    };
    if s.profile != ProfileName::User
        || recipe::mcp::validate_contract(p, s.scenario).is_err()
        || s.rollback.is_some()
        || s.interrupted_rollback.is_some()
        || s.downgrade.is_some()
    {
        return RunOutcome::Incomplete;
    }
    let completed = matches!(s.teardown, TeardownStatus::Complete)
        && s.errors.is_empty()
        && s.stages
            .iter()
            .all(|st| st.aggregate_outcome() == RunOutcome::Passed);
    if completed && !complete(s) {
        RunOutcome::Incomplete
    } else {
        RunOutcome::Passed
    }
}
pub(crate) fn artifact_outcome(r: &RunReport) -> RunOutcome {
    for s in &r.scenarios {
        if let Some(p) = &s.mcp {
            if r.schema_version < 8 || r.network != recipe::Network::None {
                return RunOutcome::Incomplete;
            }
            if r.finished_at.is_some()
                && s.aggregate_outcome() == RunOutcome::Passed
                && !super::plugin::client_artifact_present(r, &p.client_artifact)
            {
                return RunOutcome::Incomplete;
            }
        }
    }
    RunOutcome::Passed
}
