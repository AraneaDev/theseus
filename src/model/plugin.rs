//! Consistency checks for retained observations; no claim of authenticated reports.
use super::rollback_validation::{Invocation, command_role_evidence, valid_invocation};
use super::*;
use crate::recipe::{
    JsonAssertion, PluginContract,
    json::scalar,
    plugin::{CHECKPOINTS, STEPS, inventory_assertions, validate_contract},
};
fn proof<'a>(
    s: &'a ScenarioReport,
    id: &str,
    cp: Checkpoint,
) -> Option<(&'a StageReport, &'a CheckResult)> {
    let found = s
        .stages
        .iter()
        .flat_map(|stage| {
            stage
                .checks
                .iter()
                .filter(move |c| c.id == id)
                .map(move |c| (stage, c))
        })
        .collect::<Vec<_>>();
    if found.len() != 1 {
        return None;
    }
    let (stage, c) = found[0];
    if stage.checkpoint != Some(cp)
        || c.checkpoint != cp
        || c.outcome != CheckOutcome::Passed
        || c.command_ids.len() != 1
    {
        return None;
    }
    let commands = s
        .stages
        .iter()
        .flat_map(|stage| &stage.commands)
        .filter(|cmd| cmd.id == c.command_ids[0])
        .collect::<Vec<_>>();
    if commands.len() != 1 || !stage.commands.iter().any(|cmd| cmd.id == commands[0].id) {
        return None;
    }
    let cmd = commands[0];
    if !valid_invocation(&cmd.argv, Invocation::ShellScript)
        || cmd.phase != format!("check:{id}")
        || (cmd.identity.uid != 10001 || cmd.identity.gid != 10001)
        || cmd.outcome != CommandOutcome::Succeeded
        || cmd.exit_status != Some(0)
        || cmd.cancelled
        || cmd.timed_out
        || cmd.error.is_some()
    {
        return None;
    }
    Some((stage, c))
}
fn json<'a>(s: &'a ScenarioReport, id: &str, cp: Checkpoint) -> Option<&'a JsonCommandEvidence> {
    let (_, c) = proof(s, id, cp)?;
    let p = c.json_expectation.as_ref()?;
    (c.json_required && p.expected_exit == 0 && p.verified_pass()).then_some(p)
}
fn complete(s: &ScenarioReport, p: &PluginContract) -> bool {
    let mut names = vec!["baseline".to_string()];
    for (step, cp) in STEPS.into_iter().zip(CHECKPOINTS) {
        names.extend([
            step.to_string(),
            cp.to_string(),
            format!("{cp}:after_checks"),
        ]);
    }
    names.push("final".into());
    if s.stages.iter().map(|st| &st.name).ne(names.iter()) {
        return false;
    }
    let mut command_ids = std::collections::BTreeSet::new();
    for stage in &s.stages {
        if stage.snapshots.is_empty()
            || stage.snapshots.iter().any(|snap| {
                matches!(
                    snap.completeness,
                    crate::snapshot::ObservationStatus::Incomplete { .. }
                )
            })
        {
            return false;
        }
        if stage.step.is_none()
            && stage.checkpoint.is_none()
            && (!stage.commands.is_empty() || !stage.checks.is_empty())
        {
            return false;
        }
        for command in &stage.commands {
            if !command_ids.insert(&command.id)
                || command.identity.uid != 10001
                || command.identity.gid != 10001
                || command.identity.home != "/home/theseus"
                || !command.forwarded_environment.is_empty()
            {
                return false;
            }
        }
    }

    let expected = STEPS
        .into_iter()
        .zip(CHECKPOINTS)
        .flat_map(|(step, cp)| [step.to_string(), cp.to_string()])
        .collect::<Vec<_>>();
    let actual = s
        .stages
        .iter()
        .filter(|st| st.step.is_some() || st.checkpoint.is_some())
        .map(|st| st.name.clone())
        .collect::<Vec<_>>();
    if actual != expected {
        return false;
    }
    for (step, cp) in STEPS.into_iter().zip(CHECKPOINTS) {
        let stages = s
            .stages
            .iter()
            .filter(|st| st.step == Some(step))
            .collect::<Vec<_>>();
        if stages.len() != 1 {
            return false;
        }
        let stage = stages[0];
        if stage.name != step.to_string() || stage.checkpoint.is_some() || stage.commands.len() != 1
        {
            return false;
        }
        let cmd = &stage.commands[0];
        if !valid_invocation(&cmd.argv, Invocation::ShellScript)
            || cmd.phase != step.to_string()
            || cmd.outcome != CommandOutcome::Succeeded
            || cmd.exit_status != Some(0)
            || (cmd.identity.uid != 10001 || cmd.identity.gid != 10001)
            || cmd.cancelled
            || cmd.timed_out
            || cmd.error.is_some()
        {
            return false;
        }
        let checkpoints = s
            .stages
            .iter()
            .filter(|st| st.checkpoint == Some(cp))
            .collect::<Vec<_>>();
        if checkpoints.len() != 1
            || checkpoints[0].name != cp.to_string()
            || checkpoints[0].step.is_some()
        {
            return false;
        }
    }
    let Some(client) = json(s, &p.client_check, Checkpoint::BaseInstalled) else {
        return false;
    };
    if !client.assertions.iter().any(|a|matches!(a,JsonAssertion::Equals{pointer,equals_json} if pointer=="/version" && scalar(equals_json).is_ok_and(|v| v == serde_json::Value::String(p.client_version.clone())))){return false;}
    for (cp, c) in &p.checkpoints {
        let Some(inventory) = json(s, &c.inventory, *cp) else {
            return false;
        };
        if inventory_assertions(p, *cp, &inventory.assertions).is_err() {
            return false;
        }
        for id in &c.preservation {
            let Some(j) = json(s, id, *cp) else {
                return false;
            };
            if !j.assertions.iter().any(|a| {
                matches!(
                    a,
                    JsonAssertion::Equals { .. } | JsonAssertion::ArrayMatch { count: 1, .. }
                )
            }) {
                return false;
            }
        }
        for id in &c.components {
            let Some((stage, check)) = proof(s, id, *cp) else {
                return false;
            };
            if check.json_required {
                if json(s, id, *cp).is_none() {
                    return false;
                }
            } else if !command_role_evidence(stage, check)
                || check
                    .command_expectation
                    .as_ref()
                    .is_some_and(|p| p.executable.is_some())
            {
                return false;
            }
        }
    }
    true
}
pub(crate) fn scenario_outcome(s: &ScenarioReport) -> RunOutcome {
    if s.scenario != ScenarioKind::PluginLifecycle {
        return if s.plugin.is_some()
            || s.stages.iter().any(|st| {
                matches!(
                    st.step,
                    Some(StepName::DisablePlugin | StepName::EnablePlugin)
                ) || matches!(
                    st.checkpoint,
                    Some(Checkpoint::PluginDisabled | Checkpoint::PluginEnabled)
                ) || st.checks.iter().any(|c| {
                    matches!(
                        c.checkpoint,
                        Checkpoint::PluginDisabled | Checkpoint::PluginEnabled
                    )
                })
            }) {
            RunOutcome::Incomplete
        } else {
            RunOutcome::Passed
        };
    }
    let Some(p) = &s.plugin else {
        return RunOutcome::Incomplete;
    };
    if s.profile != ProfileName::User
        || validate_contract(p).is_err()
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
    if completed && !complete(s, p) {
        RunOutcome::Incomplete
    } else {
        RunOutcome::Passed
    }
}
pub(crate) fn artifact_outcome(r: &RunReport) -> RunOutcome {
    for s in r
        .scenarios
        .iter()
        .filter(|s| s.scenario == ScenarioKind::PluginLifecycle)
    {
        if r.network != crate::recipe::Network::None {
            return RunOutcome::Incomplete;
        }
        if r.finished_at.is_none() || s.aggregate_outcome() != RunOutcome::Passed {
            continue;
        }
        let Some(p) = &s.plugin else {
            return RunOutcome::Incomplete;
        };
        let targets = r
            .artifacts
            .iter()
            .filter(|a| a.name == "target")
            .collect::<Vec<_>>();
        if targets.len() != 1 {
            return RunOutcome::Incomplete;
        }
        let files = targets[0]
            .files
            .iter()
            .filter(|f| f.path == p.client_artifact)
            .collect::<Vec<_>>();
        if files.len() != 1
            || files[0].size == 0
            || files[0].sha256.len() != 64
            || !files[0].sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return RunOutcome::Incomplete;
        }
    }
    RunOutcome::Passed
}
