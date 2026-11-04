//! Native recipe inspection uses bounded record pages without launch access.
use super::{
    budget::{PageCandidate, fit_page},
    cursor::CursorRecord,
    range_writer::RangeWriter,
    roots::error,
    types::*,
};
use crate::native::recipe::ValidatedNativeRecipe;
use serde::Serialize;
#[derive(Serialize)]
#[serde(untagged)]
enum Detail<'a> {
    IntegrationRecipe(&'a crate::native::integration::recipe::NativeIntegrationRecipe),
    Requirements(&'a crate::native::integration::model::IntegrationRequirements),
    ServiceCheck(&'a crate::native::integration::model::ServiceCheckRequirement),
    Recipe(&'a crate::native::recipe::NativeRecipe),
    Command(&'a crate::native::model::CatalogCommand),
    Check(&'a crate::native::recipe::NativeCheck),
    Artifact(&'a crate::native::recipe::NativeArtifact),
    Leaves(&'a crate::native::recipe::NativeLeaves),
    Limits(&'a crate::native::recipe::NativeRunLimits),
}
struct Record<'a> {
    id: String,
    kind: RecordKind,
    collection: InspectCollection,
    detail: Detail<'a>,
}
fn records<'a>(
    recipe: &'a ValidatedNativeRecipe,
    integration: Option<&'a crate::native::integration::recipe::ValidatedIntegrationRecipe>,
) -> impl Iterator<Item = Record<'a>> {
    recipe
        .recipe
        .steps
        .iter()
        .map(|(step, c)| Record {
            id: format!("native/step/{step}"),
            kind: RecordKind::RecipeStep,
            collection: InspectCollection::Steps,
            detail: Detail::Command(c),
        })
        .chain(
            recipe
                .recipe
                .checks
                .iter()
                .enumerate()
                .map(|(i, c)| Record {
                    id: format!("native/check/{i}"),
                    kind: RecordKind::RecipeCheck,
                    collection: InspectCollection::Checks,
                    detail: Detail::Check(c),
                }),
        )
        .chain([
            Record {
                id: "native/artifact/base".into(),
                kind: RecordKind::RecipeArtifact,
                collection: InspectCollection::Artifacts,
                detail: Detail::Artifact(&recipe.recipe.base),
            },
            Record {
                id: "native/artifact/target".into(),
                kind: RecordKind::RecipeArtifact,
                collection: InspectCollection::Artifacts,
                detail: Detail::Artifact(&recipe.recipe.target),
            },
            Record {
                id: "native/leaves".into(),
                kind: RecordKind::NativeRecipe,
                collection: InspectCollection::ObservedRoots,
                detail: Detail::Leaves(&recipe.recipe.leaves),
            },
            Record {
                id: "native/limits".into(),
                kind: RecordKind::NativeRecipe,
                collection: InspectCollection::Policies,
                detail: Detail::Limits(&recipe.recipe.limits),
            },
        ])
        .chain(integration.into_iter().flat_map(|integration| {
            std::iter::once(Record {
                id: "integration/requirements".into(),
                kind: RecordKind::NativeRecipe,
                collection: InspectCollection::Policies,
                detail: Detail::Requirements(&integration.recipe.integrations),
            })
            .chain(integration.recipe.integrations.service.iter().flat_map(
                |service| {
                    service.checks.iter().enumerate().map(|(i, c)| Record {
                        id: format!("integration/check/{i}"),
                        kind: RecordKind::RecipeCheck,
                        collection: InspectCollection::Checks,
                        detail: Detail::ServiceCheck(c),
                    })
                },
            ))
        }))
}
pub(super) fn inspect(
    recipe: &ValidatedNativeRecipe,
    args: &InspectArgs,
    max: u64,
    reply: &ReplyContext,
) -> Result<InspectData, QueryError> {
    inspect_inner(recipe, None, args, max, reply)
}
pub(super) fn inspect_integration(
    recipe: &crate::native::integration::recipe::ValidatedIntegrationRecipe,
    args: &InspectArgs,
    max: u64,
    reply: &ReplyContext,
) -> Result<InspectData, QueryError> {
    inspect_inner(&recipe.core, Some(recipe), args, max, reply)
}
fn inspect_inner(
    recipe: &ValidatedNativeRecipe,
    integration: Option<&crate::native::integration::recipe::ValidatedIntegrationRecipe>,
    args: &InspectArgs,
    max: u64,
    reply: &ReplyContext,
) -> Result<InspectData, QueryError> {
    let digest = integration.map_or(recipe.digest.as_str(), |r| r.digest.as_str());
    if args
        .recipe_digest
        .as_ref()
        .is_some_and(|d| d.as_str() != digest)
    {
        return Err(error(ErrorCode::RecipeChanged));
    }
    if matches!(args.selector, InspectSelector::Overview) {
        return Ok(InspectData::NativeOverview(NativeOverviewData {
            kind: NativeOverviewKind::NativeOverview,
            recipe_path: args.recipe_path.clone(),
            recipe_digest: Digest::parse(digest).map_err(|_| error(ErrorCode::InternalError))?,
            schema: if integration.is_some() { 2 } else { 1 },
            os: recipe.recipe.os,
            role: "current_helper_account".into(),
            scenarios: recipe.recipe.scenarios.clone(),
            lifetime: Lifetime::ServerProcess,
            max_run_seconds: UInt::new(max).map_err(|_| error(ErrorCode::InternalError))?,
        }));
    }
    let mut cursor = CursorRecord::recipe(&reply.instance, args).map_err(error)?;
    let decoded = args
        .cursor
        .as_ref()
        .map(|c| CursorRecord::decode(c, &reply.instance))
        .transpose()
        .map_err(error)?;
    let revision = Revision::parse(digest).map_err(|_| error(ErrorCode::InternalError))?;
    let data = match &args.selector {
        InspectSelector::Catalog {
            profile,
            collection,
        } => {
            if profile.is_some() {
                return Err(error(ErrorCode::InvalidArguments));
            }
            let total = records(recipe, integration)
                .filter(|r| r.collection == *collection)
                .count() as u64;
            if let Some(c) = decoded {
                c.validate(&cursor, total).map_err(error)?;
                cursor = c;
            }
            let records = records(recipe, integration)
                .filter(|r| r.collection == *collection)
                .skip(cursor.next_position as usize)
                .take(usize::from(cursor.requested_limit))
                .map(|r| {
                    Ok(RecordSummary {
                        record_id: RecordId::parse(&r.id)
                            .map_err(|_| error(ErrorCode::InternalError))?,
                        record_kind: r.kind,
                        source_id: None,
                        summary: ShortPreview::new(&r.id, false),
                        outcome: None,
                    })
                })
                .collect::<Result<Vec<_>, QueryError>>()?;
            let more = cursor.next_position + (records.len() as u64) < total;
            (
                ToolData::RecipeInspect(InspectData::Catalog(CatalogPage {
                    kind: CatalogKind::Catalog,
                    revision,
                    records,
                    next_cursor: None,
                })),
                more,
            )
        }
        InspectSelector::Record { record_id } => {
            let record = if record_id.as_str() == "native_recipe" {
                Record {
                    id: "native_recipe".into(),
                    kind: RecordKind::NativeRecipe,
                    collection: InspectCollection::Policies,
                    detail: if let Some(integration) = integration {
                        Detail::IntegrationRecipe(&integration.recipe)
                    } else {
                        Detail::Recipe(&recipe.recipe)
                    },
                }
            } else {
                records(recipe, integration)
                    .find(|r| r.id == record_id.as_str())
                    .ok_or_else(|| error(ErrorCode::UnknownRecord))?
            };
            let mut counter = RangeWriter::new(0, 0);
            serde_json::to_writer(&mut counter, &record.detail)
                .map_err(|e| QueryError::new(ErrorCode::InternalError, e.into()))?;
            let total = counter.total_bytes();
            if let Some(c) = decoded {
                c.validate(&cursor, total).map_err(error)?;
                cursor = c;
            }
            let mut writer =
                RangeWriter::new(cursor.next_position, usize::from(cursor.requested_limit));
            serde_json::to_writer(&mut writer, &record.detail)
                .map_err(|e| QueryError::new(ErrorCode::InternalError, e.into()))?;
            let bytes = writer.bytes().to_vec();
            let more = cursor.next_position + (bytes.len() as u64) < total;
            (
                ToolData::RecipeInspect(InspectData::Record(RecordPage {
                    kind: RecordSelectorKind::Record,
                    revision,
                    record_id: record_id.clone(),
                    record_kind: record.kind,
                    encoding: Encoding::JsonUtf8,
                    offset: UInt::new(cursor.next_position)
                        .map_err(|_| error(ErrorCode::InvalidCursor))?,
                    bytes,
                    total_bytes: UInt::new(total)
                        .map_err(|_| error(ErrorCode::ResponseTooLarge))?,
                    next_cursor: None,
                })),
                more,
            )
        }
        _ => unreachable!(),
    };
    match fit_page(
        reply,
        PageCandidate {
            data: data.0,
            cursor,
            more_after_candidate: data.1,
        },
    )
    .map_err(|f| error(f.error.code))?
    {
        ToolData::RecipeInspect(data) => Ok(data),
        _ => Err(error(ErrorCode::InternalError)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_server::{inspection, roots::Roots};
    #[test]
    fn retained_native_catalog_is_offline_and_has_no_docker_identity_fields() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut steps = serde_json::Map::new();
        for id in ["install_target", "configure", "exercise", "uninstall"] {
            steps.insert(id.into(),serde_json::json!({"id":id,"program":id,"expected_status":{"kind":"posix_exit","code":0},"stdout_literal":null}));
        }
        let value = serde_json::json!({"kind":"theseus_native_recipe","schema":1,"os":"linux","manifest_digest":"1".repeat(64),"leaves":{"reader":"reader","app":"app","data":"data"},"base":{"length":8192,"sha256":"2".repeat(64)},"target":{"length":16384,"sha256":"3".repeat(64)},"scenarios":["clean"],"steps":steps,"checks":[{"id":"version","checkpoint":"target_installed","command":{"id":"version","program":"check_target","expected_status":{"kind":"posix_exit","code":0},"stdout_literal":"target"}},{"id":"data","checkpoint":"configured","command":{"id":"data","program":"check_data","expected_status":{"kind":"posix_exit","code":0},"stdout_literal":"data"}}],"limits":{"run_seconds":30,"inspection_seconds":30,"max_operations":128,"output_bytes":16384}});
        std::fs::write(
            root.join("native.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let roots = Roots::open(&root, &root.join("out")).unwrap();
        assert!(roots.native_target.is_none());
        let reply = ReplyContext {
            era: Era::Legacy,
            id: RpcId::Integer(1),
            instance: Instance::parse(&"a".repeat(32)).unwrap(),
        };
        let mut args = InspectArgs {
            recipe_path: "native.json".into(),
            recipe_digest: None,
            selector: InspectSelector::Overview,
            limit: None,
            cursor: None,
        };
        let InspectData::NativeOverview(data) =
            inspection::inspect(&roots, &args, 30, &reply).unwrap()
        else {
            panic!()
        };
        let json = serde_json::to_value(&data).unwrap();
        assert_eq!(json["role"], "current_helper_account");
        assert!(json.get("image").is_none());
        assert!(json.get("profiles").is_none());
        args.recipe_digest = Some(data.recipe_digest);
        args.selector = InspectSelector::Catalog {
            collection: InspectCollection::Steps,
            profile: None,
        };
        args.limit = Some(UInt::new(1).unwrap());
        let InspectData::Catalog(page) = inspection::inspect(&roots, &args, 30, &reply).unwrap()
        else {
            panic!()
        };
        assert_eq!(page.records.len(), 1);
        assert!(page.next_cursor.is_some());
        assert!(
            page.records[0]
                .record_id
                .as_str()
                .starts_with("native/step/")
        );
        args.selector = InspectSelector::Record {
            record_id: page.records[0].record_id.clone(),
        };
        args.limit = Some(UInt::new(4096).unwrap());
        let InspectData::Record(page) = inspection::inspect(&roots, &args, 30, &reply).unwrap()
        else {
            panic!()
        };
        let command: serde_json::Value = serde_json::from_slice(&page.bytes).unwrap();
        assert_eq!(command["expected_status"]["kind"], "posix_exit");
        assert!(command.get("argv").is_none());
        args.selector = InspectSelector::Catalog {
            collection: InspectCollection::Steps,
            profile: Some(crate::model::ProfileName::User),
        };
        args.limit = Some(UInt::new(1).unwrap());
        assert!(inspection::inspect(&roots, &args, 30, &reply).is_err());
    }
}
