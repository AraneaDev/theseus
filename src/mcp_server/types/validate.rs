//! ToolCall argument validation.
use super::*;

fn path_valid(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && RelativePath::parse(path).is_ok()
        && path.split('/').all(|component| {
            if component.ends_with(['.', ' '])
                || component
                    .chars()
                    .any(|c| c < ' ' || "\\:<>\"|?*".contains(c))
            {
                return false;
            }
            let stem = component
                .split('.')
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            !matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            ) && !["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|n| {
                    matches!(
                        n,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                })
            })
        })
}
fn bounded_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 4096
}
fn limit_valid(limit: Option<UInt>, maximum: u64) -> bool {
    limit.is_none_or(|n| (1..=maximum).contains(&n.get()))
}
fn same_instance(instance: &Instance, handle: &Handle) -> bool {
    handle
        .as_str()
        .split_once(':')
        .is_some_and(|(i, _)| i == instance.as_str())
}
impl ToolCall {
    pub fn validate(&self) -> Result<(), ArgumentError> {
        let valid = match self {
            Self::RecipeInspect(a) => {
                path_valid(&a.recipe_path)
                    && match &a.selector {
                        InspectSelector::Overview => a.limit.is_none() && a.cursor.is_none(),
                        InspectSelector::Catalog {
                            collection,
                            profile,
                        } => {
                            a.recipe_digest.is_some()
                                && limit_valid(a.limit, 32)
                                && (profile.is_none()
                                    || !matches!(
                                        collection,
                                        InspectCollection::Artifacts
                                            | InspectCollection::ObservedRoots
                                    ))
                        }
                        InspectSelector::Record { .. } => {
                            a.recipe_digest.is_some() && limit_valid(a.limit, 4096)
                        }
                    }
            }
            Self::RunStart(a) => {
                path_valid(&a.recipe_path)
                    && a.profiles.len() <= 2
                    && (1..=9).contains(&a.scenarios.len())
                    && a.profiles
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        == a.profiles.len()
                    && a.scenarios
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        == a.scenarios.len()
                    && (1..=16_777_216).contains(&a.log_limit.get())
            }
            Self::RunList(a) => limit_valid(a.limit, 32),
            Self::RunStatus(a) => same_instance(&a.server_instance, &a.handle),
            Self::RunCancel(a) => same_instance(&a.server_instance, &a.handle),
            Self::RunEvidence(a) => {
                same_instance(&a.server_instance, &a.handle)
                    && (a.cursor.is_none() || a.revision.is_some())
                    && match &a.selector {
                        EvidenceSelector::Root(_) | EvidenceSelector::Artifact(_) => {
                            limit_valid(a.limit, 32)
                        }
                        EvidenceSelector::Scenario(s) => {
                            bounded_id(&s.scenario_id) && limit_valid(a.limit, 32)
                        }
                        EvidenceSelector::Stage(s) => {
                            bounded_id(&s.scenario_id) && limit_valid(a.limit, 32)
                        }
                        EvidenceSelector::Snapshot(s) => {
                            bounded_id(&s.scenario_id) && limit_valid(a.limit, 32)
                        }
                        EvidenceSelector::Record(_) => {
                            a.revision.is_some() && limit_valid(a.limit, 4096)
                        }
                        EvidenceSelector::Log(s) => {
                            bounded_id(&s.command_id)
                                && a.revision.is_some()
                                && limit_valid(a.limit, 4096)
                        }
                    }
            }
        };
        if valid { Ok(()) } else { Err(ArgumentError) }
    }
}
