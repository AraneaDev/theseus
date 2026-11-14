//! Recipe preview as labelled, styled lines. Building it never executes anything;
//! the plain-text form is the same content without styling.
use super::{App, safe_text};
use crate::{
    model::*,
    recipe::{CommandSpec, Expectation, JsonAssertion, Network},
};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Plain,
    Heading,
    Label,
    Name,
    Dim,
    Selected,
    Unselected,
}
impl Tone {
    fn style(self) -> Style {
        match self {
            Self::Plain => Style::default(),
            Self::Heading => Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            Self::Label => Style::default().add_modifier(Modifier::BOLD),
            Self::Name => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            Self::Dim => Style::default().fg(Color::DarkGray),
            Self::Selected => Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
            Self::Unselected => Style::default().fg(Color::DarkGray),
        }
    }
}

/// Lines of toned segments. Pushed text may contain newlines.
pub(super) struct Doc {
    lines: Vec<Vec<(Tone, String)>>,
}
impl Doc {
    fn new() -> Self {
        Self {
            lines: vec![Vec::new()],
        }
    }
    fn push(&mut self, tone: Tone, text: &str) -> &mut Self {
        let mut parts = text.split('\n');
        if let Some(first) = parts.next()
            && !first.is_empty()
        {
            self.lines
                .last_mut()
                .unwrap()
                .push((tone, safe_text(first)));
        }
        for part in parts {
            self.lines.push(Vec::new());
            if !part.is_empty() {
                self.lines.last_mut().unwrap().push((tone, safe_text(part)));
            }
        }
        self
    }
    fn plain(&mut self, text: &str) -> &mut Self {
        self.push(Tone::Plain, text)
    }
    fn dim(&mut self, text: &str) -> &mut Self {
        self.push(Tone::Dim, text)
    }
    fn label(&mut self, label: &str, value: &str) -> &mut Self {
        self.push(Tone::Label, label).push(Tone::Plain, value)
    }
    fn heading(&mut self, text: &str) -> &mut Self {
        self.push(Tone::Heading, text).plain("\n")
    }
    pub(super) fn text(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.iter().map(|(_, s)| s.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }
    pub(super) fn lines(&self) -> Vec<Line<'static>> {
        self.lines
            .iter()
            .map(|line| {
                Line::from(
                    line.iter()
                        .map(|(tone, s)| Span::styled(s.clone(), tone.style()))
                        .collect::<Vec<_>>(),
                )
            })
            .collect()
    }
}

fn list<T: std::fmt::Display>(items: impl IntoIterator<Item = T>) -> String {
    let text = items
        .into_iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if text.is_empty() { "none".into() } else { text }
}
fn env(env: &BTreeMap<String, String>) -> String {
    env.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}
fn indent(text: &str, by: &str) -> String {
    text.lines()
        .map(|line| format!("{by}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}
/// Environment and forwarded names as metadata; empty parts are omitted.
fn environment(doc: &mut Doc, pad: &str, vars: &BTreeMap<String, String>, pass: &[String]) {
    if !vars.is_empty() {
        doc.dim(&format!("{pad}env {}\n", env(vars)));
    }
    if !pass.is_empty() {
        doc.dim(&format!("{pad}pass_env {}\n", pass.join(", ")));
    }
}
fn step(doc: &mut Doc, name: &StepName, command: &CommandSpec) {
    doc.push(Tone::Name, &format!("  {name}"))
        .dim(&format!("  {}s\n", command.timeout_seconds));
    environment(doc, "    ", &command.env, &command.pass_env);
    doc.plain(&indent(&command.script, "    ")).plain("\n");
}
fn assertion(a: &JsonAssertion) -> String {
    match a {
        JsonAssertion::Equals {
            pointer,
            equals_json,
        } => format!("{pointer:?} equals_json {equals_json}"),
        JsonAssertion::Absent { pointer } => format!("{pointer:?} absent"),
        JsonAssertion::ArrayMatch {
            pointer,
            identity,
            count,
            values,
        } => format!(
            "{pointer:?} array_match count={count} identity [{}] values [{}]",
            identity
                .iter()
                .map(|p| format!("{:?} equals_json {}", p.pointer, p.equals_json))
                .collect::<Vec<_>>()
                .join(", "),
            values
                .iter()
                .map(|p| format!("{:?} equals_json {}", p.pointer, p.equals_json))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
fn check(doc: &mut Doc, check: &crate::recipe::Check) {
    doc.dim("  check ")
        .push(Tone::Name, &check.id)
        .dim(&format!(" @ {}: ", check.checkpoint));
    let pad = "      ";
    match &check.expectation {
        Expectation::JsonCommand {
            script,
            timeout_seconds,
            expected_exit,
            max_stdout_bytes,
            env: vars,
            pass_env,
            assertions,
        } => {
            doc.plain("JSON command\n")
                .label(&format!("{pad}script: "), script)
                .plain("\n")
                .dim(&format!(
                    "{pad}expected exit: {expected_exit}; stdout cap: {max_stdout_bytes} bytes; timeout: {timeout_seconds}s\n"
                ));
            environment(doc, pad, vars, pass_env);
            for a in assertions {
                doc.dim(&format!("{pad}assert "))
                    .plain(&assertion(a))
                    .plain("\n");
            }
        }
        Expectation::Command {
            script,
            timeout_seconds,
            env: vars,
            pass_env,
            expected_exit,
            stdout_contains,
        } => {
            doc.plain("command\n")
                .label(&format!("{pad}script: "), script)
                .plain("\n")
                .dim(&format!(
                    "{pad}expected exit: {expected_exit}; timeout: {timeout_seconds}s\n"
                ));
            if let Some(marker) = stdout_contains {
                doc.dim(&format!("{pad}stdout contains {marker:?}\n"));
            }
            environment(doc, pad, vars, pass_env);
        }
        Expectation::Executable {
            name,
            resolved_path,
            args,
            timeout_seconds,
            env: vars,
            pass_env,
            expected_exit,
            stdout_contains,
        } => {
            doc.plain("executable ")
                .push(Tone::Label, name)
                .plain(&format!(" → {resolved_path}\n"));
            if !args.is_empty() {
                doc.label(&format!("{pad}args: "), &args.join(" "))
                    .plain("\n");
            }
            doc.dim(&format!(
                "{pad}expected exit: {expected_exit}; timeout: {timeout_seconds}s\n"
            ));
            if let Some(marker) = stdout_contains {
                doc.dim(&format!("{pad}stdout contains {marker:?}\n"));
            }
            environment(doc, pad, vars, pass_env);
        }
        Expectation::McpStdio { probe } => {
            doc.plain(&format!(
                "MCP stdio {} {}\n",
                probe.server_name, probe.server_version
            ))
            .label(&format!("{pad}argv: "), &probe.argv.join(" "))
            .plain("\n")
            .dim(&format!(
                "{pad}tools: {}; timeout: {}s\n",
                list(&probe.tools),
                probe.timeout_seconds
            ));
            environment(doc, pad, &probe.env, &[]);
        }
        Expectation::Absent { path } => {
            doc.plain(&format!("absent {path}\n"));
        }
        Expectation::Present {
            path,
            expected_kind,
        } => {
            doc.plain(&format!("present {path}"));
            if let Some(kind) = expected_kind {
                doc.dim(&format!(" ({kind})"));
            }
            doc.plain("\n");
        }
        Expectation::Preserved { path } => {
            doc.plain(&format!("preserved {path}\n"));
        }
        Expectation::Unchanged { path } => {
            doc.plain(&format!("unchanged {path}\n"));
        }
        Expectation::Changed { path } => {
            doc.plain(&format!("changed {path}\n"));
        }
    }
}
/// Available choices with the current selection bracketed and highlighted.
fn choices<T: PartialEq + std::fmt::Display>(
    doc: &mut Doc,
    label: &str,
    all: &[T],
    selected: &[T],
) {
    doc.push(Tone::Label, label);
    for (i, item) in all.iter().enumerate() {
        if i > 0 {
            doc.plain(" ");
        }
        if selected.contains(item) {
            doc.push(Tone::Selected, &format!("[{item}]"));
        } else {
            doc.push(Tone::Unselected, &format!(" {item} "));
        }
    }
}

pub(super) fn build(app: &App) -> Doc {
    let mut doc = Doc::new();
    let (Some(recipe), Some(options)) = (&app.recipe, &app.options) else {
        return doc;
    };
    let r = &recipe.recipe;
    doc.push(Tone::Heading, "Preview: ")
        .push(Tone::Heading, &r.name)
        .plain("\n")
        .label("Image: ", &r.image)
        .label(
            "   Network: ",
            match r.network {
                Network::None => "none",
                Network::Bridge => "bridge",
            },
        )
        .label("   Pull: ", &options.pull.to_string())
        .plain("\n");
    let profiles = r.profiles.keys().copied().collect::<Vec<_>>();
    choices(&mut doc, "Profiles:  ", &profiles, &options.profiles);
    doc.plain("\n");
    choices(
        &mut doc,
        "Scenarios: ",
        &recipe.declared_scenarios(),
        &options.scenarios,
    );
    doc.plain("\n")
        .push(Tone::Label, "Will run:  ")
        .push(
            Tone::Name,
            &format!("{} / {}", list(&options.profiles), list(&options.scenarios)),
        )
        .dim("   (p profiles · s scenarios · r run explicitly)\n")
        .dim("Runtime UID/GID 10001:10001 | system installation 0:0\nHOME=/home/theseus XDG_CONFIG_HOME=/home/theseus/.config\nXDG_DATA_HOME=/home/theseus/.local/share XDG_CACHE_HOME=/home/theseus/.cache\nPATH=/home/theseus/.local/bin:/usr/local/bin:/usr/bin:/bin\nWorking directory: /workspace | root installation HOME=/root\n")
        .label("Observe: ", &list(&r.observe))
        .plain("\n")
        .label(
            "Inputs: ",
            &format!(
                "{}target → {}",
                r.artifacts
                    .base
                    .as_ref()
                    .map_or(String::new(), |b| format!("base → {}, ", b.display())),
                r.artifacts.target.display()
            ),
        )
        .plain("\n");
    for name in &options.profiles {
        if let Some(profile) = r.profiles.get(name) {
            doc.plain("\n").heading(&format!("[{name}]"));
            scenario_notes(&mut doc, options, profile);
            if !profile.steps.is_empty() {
                doc.push(Tone::Label, "Steps\n");
            }
            for (name, command) in &profile.steps {
                step(&mut doc, name, command);
            }
            if !profile.checks.is_empty() {
                doc.push(Tone::Label, "Checks\n");
            }
            for c in &profile.checks {
                check(&mut doc, c);
            }
        }
    }
    doc
}

fn scenario_notes(
    doc: &mut Doc,
    options: &crate::engine::RunOptions,
    profile: &crate::recipe::Profile,
) {
    if options.scenarios.contains(&ScenarioKind::PluginLifecycle)
        && let Some(plugin) = &profile.plugin
    {
        doc.plain(&format!(
            "Plugin lifecycle: {} | scope: {} | {} -> {}\nClient expected version: {} | artifact: {} (file SHA256/size retained in run evidence)\nProof boundary: declared container observations, no authentication/model invocation or real hook dispatch; manual hook rehearsal only. Disposable HOME/config/project; no forwarded environment, network none.\n",
            plugin.id,
            plugin.scope.as_str(),
            plugin.base_version,
            plugin.target_version,
            plugin.client_version,
            plugin.client_artifact
        ))
        .dim(&format!("Plugin contract: {plugin:#?}\n"));
    }
    if options.scenarios.iter().any(|kind| {
        matches!(
            kind,
            ScenarioKind::InterruptedInstall | ScenarioKind::InterruptedUpgrade
        )
    }) {
        doc.plain("Interruption: ContainerSigkill at synchronous THESEUS_INTERRUPT_BARRIER; abrupt whole-container stop, same-ID restart without setup or input copying.\nPartial interrupted checks run after restart and before explicit recovery.\nUnchanged compares before_interrupt -> interrupted/recovered (before checks). Preserved uses configured.\n");
        for (kind, interrupt, recover) in [
            (
                ScenarioKind::InterruptedInstall,
                StepName::InterruptInstall,
                StepName::RecoverInstall,
            ),
            (
                ScenarioKind::InterruptedUpgrade,
                StepName::InterruptUpgrade,
                StepName::RecoverUpgrade,
            ),
        ] {
            if options.scenarios.contains(&kind) {
                doc.plain(match kind {
                    ScenarioKind::InterruptedInstall => {
                        "before_interrupt: before initial installation.\n"
                    }
                    ScenarioKind::InterruptedUpgrade => "before_interrupt: after first exercise.\n",
                    _ => unreachable!(),
                });
                if let Some(command) = profile.steps.get(&interrupt) {
                    doc.plain(&format!(
                        "{kind} barrier/stop budget: {}s\n",
                        command.timeout_seconds
                    ));
                }
                if let Some(command) = profile.steps.get(&recover) {
                    doc.plain(&format!("Explicit recovery command: {}\n", command.script));
                }
            }
        }
    }
    if options
        .scenarios
        .contains(&ScenarioKind::InterruptedRollback)
    {
        doc.plain("Interrupted rollback artifact roles: base = known-good; target = interrupted candidate.\nInterruption: ContainerSigkill at synchronous THESEUS_INTERRUPT_BARRIER; abrupt whole-container stop, same-ID restart without setup or input copying.\nOriginal reference: before_upgrade after KnownGood checks; Changed: before_interrupt -> interrupted before checks; restoration: before_upgrade -> rolled_back.\nFresh before_rollback revalidates backup after Interrupted check effects.\n");
        if let Some(command) = profile.steps.get(&StepName::InterruptUpgrade) {
            doc.plain(&format!(
                "Candidate barrier/stop budget: {}s\n",
                command.timeout_seconds
            ));
        }
        if let Some(command) = profile.steps.get(&StepName::Rollback) {
            doc.plain(&format!("Explicit rollback command: {}\n", command.script));
        }
        if let Some(policy) = &profile.interrupted_rollback {
            recovery_checks(doc, &policy.recovery);
            restoration(doc, &policy.recovery.restoration, "Interrupted", false);
        }
    }
    if options.scenarios.contains(&ScenarioKind::Rollback) {
        doc.plain("Rollback artifact roles: base = known-good; target = failed candidate.\nKnown-good reference: before_upgrade after KnownGood checks; Changed: before_attempt -> upgrade_failed; restore: before_upgrade -> rolled_back.\n");
        if let Some(policy) = &profile.rollback {
            doc.plain(&format!(
                "Expected failure: attempt_upgrade exit={} marker={:?}\n",
                policy.failure.expected_exit, policy.failure.stdout_contains,
            ));
            recovery_checks(doc, &policy.recovery);
            restoration(doc, &policy.recovery.restoration, "UpgradeFailed", true);
        }
    }
    if options.scenarios.contains(&ScenarioKind::Downgrade) {
        doc.plain("Downgrade artifact roles: target = newer installed; base = older requested.\nCompare before_downgrade (after newer exercise) to the declared policy checkpoint before checks. Preserved uses configured.\n");
        if let Some(policy) = &profile.downgrade {
            match policy {
                crate::recipe::DowngradeContract::Compatible { data_checks } => {
                    doc.plain(&format!(
                        "Policy: compatible; older runtime and data reader checks: {}\n",
                        list(data_checks)
                    ));
                }
                crate::recipe::DowngradeContract::SafeRejection {
                    expected_exit,
                    stdout_contains,
                    binary_checks,
                    data_checks,
                } => {
                    doc.plain(&format!(
                        "Policy: safe_rejection; expected refusal exit={expected_exit}, stdout marker={stdout_contains:?}\nBinary scopes: {}; data scopes: {}\n",
                        list(binary_checks),
                        list(data_checks)
                    ));
                }
            }
        }
        for step in [StepName::Exercise, StepName::ExerciseDowngraded] {
            if let Some(command) = profile.steps.get(&step) {
                doc.plain(&format!("Downgrade runtime {step}: {}\n", command.script));
            }
        }
    }
    if options.scenarios.contains(&ScenarioKind::Reinstall) {
        doc.plain("Reinstall rehearsal: install_target, configure, exercise_before_reinstall, before_reinstall, reinstall, reinstalled, exercise_after_reinstall, uninstall.\nBoth exercise stages run the same exercise command as UID/GID 10001:10001.\n");
        if let Some(command) = profile.steps.get(&StepName::Reinstall) {
            doc.plain(&format!(
                "Repeated target installation command: {}\n",
                command.script
            ));
        }
        let stable: Vec<_> = profile
            .checks
            .iter()
            .filter(|check| check.checkpoint == Checkpoint::Reinstalled)
            .filter_map(|check| match &check.expectation {
                Expectation::Unchanged { path } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        doc.plain(&format!(
            "Declared stable scopes (path and descendants): {}\nCompare before_reinstall (after first exercise) to reinstalled (before checks and second exercise).\n",
            list(stable)
        ));
    }
}
fn recovery_checks(doc: &mut Doc, recovery: &crate::recipe::RollbackRecoveryContract) {
    doc.plain(&format!(
        "Changed checks: {}; binary checks: {}; runtime checks: {}; data readers: {}\n",
        list(&recovery.change_checks),
        list(&recovery.binary_checks),
        list(&recovery.runtime_checks),
        list(&recovery.data_checks)
    ));
}
fn restoration(
    doc: &mut Doc,
    policy: &crate::recipe::RestorationPolicy,
    changed_phase: &str,
    known_good_reference: bool,
) {
    match policy {
        crate::recipe::RestorationPolicy::BinaryOnly => {
            doc.plain(if known_good_reference {
                "Policy: binary_only; known-good runtime must read retained data.\n"
            } else {
                "Policy: binary_only; known-good runtime must read retained partially changed data.\n"
            });
        }
        crate::recipe::RestorationPolicy::RestoreData {
            backup_root,
            scopes,
        } => {
            doc.plain(&format!(
                "Policy: restore_data; backup root {backup_root}\nDerived checks: __theseus_rollback_root_absent (Backup before command{}); __theseus_rollback_backup_removed (BackupRemoved)\n",
                if known_good_reference {
                    ", before_upgrade reference"
                } else {
                    ""
                }
            ));
            for scope in scopes {
                doc.plain(&format!(
                    "Scope {}: {} -> {backup_root}/{}\n",
                    scope.id, scope.source, scope.stored
                ));
                for (phase, label) in [
                    ("BackupCreated", "source_stable"),
                    ("BackupCreated", "backup_matches"),
                    (changed_phase, "backup_intact"),
                    ("before_rollback", "backup_ready"),
                    ("RolledBack", "restored"),
                    ("RolledBack", "backup_retained"),
                ] {
                    doc.dim(&format!(
                        "Derived {phase}: __theseus_rollback_{label}_{}\n",
                        scope.id
                    ));
                }
            }
        }
    }
}
