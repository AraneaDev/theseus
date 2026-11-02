//! Borrowed typed recipe projections; no complete serialized detail is allocated.
use super::{roots::error, types::*};
use crate::{
    model::{ProfileName, StepName},
    recipe::{self, Check, CommandSpec, Expectation, ValidatedRecipe},
};
use serde::{Serialize, Serializer, ser::SerializeMap};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Owner {
    Step { step: StepName },
    Check { index: usize },
}
pub(super) enum Record<'a> {
    Step(ProfileName, StepName, &'a CommandSpec),
    Check(ProfileName, usize, &'a Check),
    Artifact(&'static str, &'a std::path::Path),
    Environment(usize, ProfileName, Owner, &'a str, &'static str),
    Observe(usize, &'a str),
    Policy(ProfileName, Policy<'a>),
}
pub(super) enum Policy<'a> {
    Mcp(&'a recipe::McpLifecycleContract),
    Plugin(&'a recipe::PluginContract),
    Downgrade(&'a recipe::DowngradeContract),
    Rollback(&'a recipe::RollbackContract),
    InterruptedRollback(&'a recipe::InterruptedRollbackContract),
}
impl Policy<'_> {
    fn kind(&self) -> &'static str {
        match self {
            Self::Mcp(_) => "mcp",
            Self::Plugin(_) => "plugin",
            Self::Downgrade(_) => "downgrade",
            Self::Rollback(_) => "rollback",
            Self::InterruptedRollback(_) => "interrupted_rollback",
        }
    }
}
impl Serialize for Policy<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Mcp(p) => p.serialize(s),
            Self::Plugin(p) => p.serialize(s),
            Self::Downgrade(p) => p.serialize(s),
            Self::Rollback(p) => p.serialize(s),
            Self::InterruptedRollback(p) => p.serialize(s),
        }
    }
}
fn names(env: &BTreeMap<String, String>) -> Vec<&str> {
    env.keys().map(String::as_str).collect()
}
fn forwarded(pass: &[String]) -> Vec<&str> {
    pass.iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn check_env(check: &Check) -> Option<(&BTreeMap<String, String>, &[String])> {
    match &check.expectation {
        Expectation::Command { env, pass_env, .. }
        | Expectation::JsonCommand { env, pass_env, .. }
        | Expectation::Executable { env, pass_env, .. } => Some((env, pass_env)),
        Expectation::McpStdio { probe } => Some((&probe.env, &[])),
        _ => None,
    }
}
impl Record<'_> {
    pub fn id(&self) -> String {
        match self {
            Self::Step(p, s, _) => format!("p/{p}/step/{s}"),
            Self::Check(p, i, _) => format!("p/{p}/check/{i}"),
            Self::Artifact(n, _) => format!("artifact/{n}"),
            Self::Environment(i, ..) => format!("env/{i}"),
            Self::Observe(i, _) => format!("observe/{i}"),
            Self::Policy(p, k) => format!("p/{p}/policy/{}", k.kind()),
        }
    }
    pub fn kind(&self) -> RecordKind {
        match self {
            Self::Step(..) => RecordKind::RecipeStep,
            Self::Check(..) => RecordKind::RecipeCheck,
            Self::Artifact(..) => RecordKind::RecipeArtifact,
            Self::Environment(..) => RecordKind::RecipeEnvironment,
            Self::Observe(..) => RecordKind::RecipeObservedRoot,
            Self::Policy(..) => RecordKind::RecipePolicy,
        }
    }
    pub fn profile(&self) -> Option<ProfileName> {
        match self {
            Self::Step(p, ..)
            | Self::Check(p, ..)
            | Self::Environment(_, p, ..)
            | Self::Policy(p, ..) => Some(*p),
            _ => None,
        }
    }
    pub fn collection(&self) -> InspectCollection {
        match self {
            Self::Step(..) => InspectCollection::Steps,
            Self::Check(..) => InspectCollection::Checks,
            Self::Artifact(..) => InspectCollection::Artifacts,
            Self::Environment(..) => InspectCollection::Environment,
            Self::Observe(..) => InspectCollection::ObservedRoots,
            Self::Policy(..) => InspectCollection::Policies,
        }
    }
    pub fn summary(&self) -> Result<RecordSummary, QueryError> {
        let id = self.id();
        let (source, summary) = match self {
            Self::Step(_, _, cmd) => (None, cmd.script.as_str()),
            Self::Check(_, _, c) => (Some(c.id.as_str()), c.id.as_str()),
            Self::Artifact(n, _) => (None, *n),
            Self::Environment(_, _, _, name, _) => (None, *name),
            Self::Observe(_, path) => (None, *path),
            Self::Policy(_, policy) => (None, policy.kind()),
        };
        Ok(RecordSummary {
            record_id: RecordId::parse(&id).map_err(|_| error(ErrorCode::InternalError))?,
            record_kind: self.kind(),
            source_id: source.map(|s| ShortPreview::new(s, false)),
            summary: ShortPreview::new(summary, false),
            outcome: None,
        })
    }
}
impl Serialize for Record<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(None)?;
        match self {
            Self::Step(profile, step, c) => {
                map.serialize_entry("profile", profile)?;
                map.serialize_entry("step", step)?;
                map.serialize_entry("script", &c.script)?;
                map.serialize_entry("timeout_seconds", &c.timeout_seconds)?;
                map.serialize_entry("declared_env_names", &names(&c.env))?;
                map.serialize_entry("forwarded_env_names", &forwarded(&c.pass_env))?;
            }
            Self::Check(profile, index, check) => {
                map.serialize_entry("profile", profile)?;
                map.serialize_entry("index", index)?;
                map.serialize_entry("check", &SanitizedCheck(check))?;
            }
            Self::Artifact(name, path) => {
                map.serialize_entry("name", name)?;
                map.serialize_entry("declared_path", path)?;
            }
            Self::Environment(_, profile, owner, name, source) => {
                map.serialize_entry("profile", profile)?;
                map.serialize_entry("owner", owner)?;
                map.serialize_entry("name", name)?;
                map.serialize_entry("source", source)?;
            }
            Self::Observe(index, path) => {
                map.serialize_entry("index", index)?;
                map.serialize_entry("path", path)?;
            }
            Self::Policy(profile, policy) => {
                map.serialize_entry("profile", profile)?;
                map.serialize_entry("kind", policy.kind())?;
                map.serialize_entry("policy", policy)?;
            }
        }
        map.end()
    }
}
struct SanitizedProbe<'a>(&'a crate::mcp::McpProbe);
impl Serialize for SanitizedProbe<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let p = self.0;
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("argv", &p.argv)?;
        m.serialize_entry("declared_env_names", &names(&p.env))?;
        m.serialize_entry("timeout_seconds", &p.timeout_seconds)?;
        m.serialize_entry("protocol", &p.protocol)?;
        m.serialize_entry("server_name", &p.server_name)?;
        m.serialize_entry("server_version", &p.server_version)?;
        m.serialize_entry("tools", &p.tools)?;
        m.end()
    }
}
struct SanitizedCheck<'a>(&'a Check);
impl Serialize for SanitizedCheck<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let c = self.0;
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("id", &c.id)?;
        m.serialize_entry("checkpoint", &c.checkpoint)?;
        macro_rules! field {
            ($key:literal,$value:expr) => {
                m.serialize_entry($key, $value)?
            };
        }
        match &c.expectation {
            Expectation::McpStdio { probe } => {
                field!("kind", &"mcp_stdio");
                field!("probe", &SanitizedProbe(probe));
            }
            Expectation::Command {
                script,
                timeout_seconds,
                env,
                pass_env,
                expected_exit,
                stdout_contains,
            } => {
                field!("kind", &"command");
                field!("script", script);
                field!("timeout_seconds", timeout_seconds);
                field!("declared_env_names", &names(env));
                field!("pass_env", pass_env);
                field!("expected_exit", expected_exit);
                field!("stdout_contains", stdout_contains);
            }
            Expectation::JsonCommand {
                script,
                timeout_seconds,
                env,
                pass_env,
                expected_exit,
                max_stdout_bytes,
                assertions,
            } => {
                field!("kind", &"json_command");
                field!("script", script);
                field!("timeout_seconds", timeout_seconds);
                field!("declared_env_names", &names(env));
                field!("pass_env", pass_env);
                field!("expected_exit", expected_exit);
                field!("max_stdout_bytes", max_stdout_bytes);
                field!("assertions", assertions);
            }
            Expectation::Executable {
                name,
                resolved_path,
                args,
                timeout_seconds,
                env,
                pass_env,
                expected_exit,
                stdout_contains,
            } => {
                field!("kind", &"executable");
                field!("name", name);
                field!("resolved_path", resolved_path);
                field!("args", args);
                field!("timeout_seconds", timeout_seconds);
                field!("declared_env_names", &names(env));
                field!("pass_env", pass_env);
                field!("expected_exit", expected_exit);
                field!("stdout_contains", stdout_contains);
            }
            Expectation::Absent { path } => {
                field!("kind", &"absent");
                field!("path", path);
            }
            Expectation::Present {
                path,
                expected_kind,
            } => {
                field!("kind", &"present");
                field!("path", path);
                field!("expected_kind", expected_kind);
            }
            Expectation::Preserved { path } => {
                field!("kind", &"preserved");
                field!("path", path);
            }
            Expectation::Unchanged { path } => {
                field!("kind", &"unchanged");
                field!("path", path);
            }
            Expectation::Changed { path } => {
                field!("kind", &"changed");
                field!("path", path);
            }
        }
        m.end()
    }
}
pub(super) fn records(validated: &ValidatedRecipe) -> Vec<Record<'_>> {
    let recipe = &validated.recipe;
    let mut result = Vec::new();
    let mut env_index = 0;
    for (&profile, p) in &recipe.profiles {
        for (&step, cmd) in &p.steps {
            result.push(Record::Step(profile, step, cmd));
            for (values, source) in [
                (names(&cmd.env), "literal"),
                (forwarded(&cmd.pass_env), "forwarded"),
            ] {
                for name in values {
                    result.push(Record::Environment(
                        env_index,
                        profile,
                        Owner::Step { step },
                        name,
                        source,
                    ));
                    env_index += 1;
                }
            }
        }
        for (index, check) in p.checks.iter().enumerate() {
            result.push(Record::Check(profile, index, check));
            if let Some((env, pass)) = check_env(check) {
                for (values, source) in [(names(env), "literal"), (forwarded(pass), "forwarded")] {
                    for name in values {
                        result.push(Record::Environment(
                            env_index,
                            profile,
                            Owner::Check { index },
                            name,
                            source,
                        ));
                        env_index += 1;
                    }
                }
            }
        }
        if let Some(p) = &p.mcp {
            result.push(Record::Policy(profile, Policy::Mcp(p)));
        }
        if let Some(p) = &p.plugin {
            result.push(Record::Policy(profile, Policy::Plugin(p)));
        }
        if let Some(p) = &p.downgrade {
            result.push(Record::Policy(profile, Policy::Downgrade(p)));
        }
        if let Some(p) = &p.rollback {
            result.push(Record::Policy(profile, Policy::Rollback(p)));
        }
        if let Some(p) = &p.interrupted_rollback {
            result.push(Record::Policy(profile, Policy::InterruptedRollback(p)));
        }
    }
    if let Some(path) = &recipe.artifacts.base {
        result.push(Record::Artifact("base", path));
    }
    result.push(Record::Artifact("target", &recipe.artifacts.target));
    for (index, path) in recipe.observe.iter().enumerate() {
        result.push(Record::Observe(index, path));
    }
    result
}
