//! Read-only bounded inspection of sanitized validated recipes.
use super::{
    budget::{PageCandidate, fit_page},
    cursor::CursorRecord,
    inspection_records,
    range_writer::RangeWriter,
    roots::{Roots, error, read_recipe},
    types::*,
};

pub(crate) fn inspect(
    roots: &Roots,
    args: &InspectArgs,
    max_run_seconds: u64,
    reply: &ReplyContext,
) -> Result<InspectData, QueryError> {
    ToolCall::RecipeInspect(args.clone())
        .validate()
        .map_err(|_| error(ErrorCode::InvalidArguments))?;
    // Decode before using any position, even if the recipe has subsequently changed.
    let decoded = args
        .cursor
        .as_ref()
        .map(|c| CursorRecord::decode(c, &reply.instance))
        .transpose()
        .map_err(error)?;
    let validated = read_recipe(roots, &args.recipe_path)?;
    if args
        .recipe_digest
        .as_ref()
        .is_some_and(|d| d.as_str() != validated.digest)
    {
        return Err(error(ErrorCode::RecipeChanged));
    }
    let recipe = &validated.recipe;
    if matches!(args.selector, InspectSelector::Overview) {
        let path_preview = |p: &std::path::Path| {
            let display = p.to_string_lossy();
            TextPreview::new(&display, matches!(display, std::borrow::Cow::Owned(_)))
        };
        return Ok(InspectData::Overview(OverviewData {
            kind: OverviewKind::Overview,
            recipe_path: args.recipe_path.clone(),
            recipe_digest: Digest::parse(&validated.digest)
                .map_err(|_| error(ErrorCode::InternalError))?,
            schema_version: u8::try_from(recipe.schema_version)
                .map_err(|_| error(ErrorCode::InternalError))?,
            name: TextPreview::new(&recipe.name, false),
            image: TextPreview::new(&recipe.image, false),
            network: match recipe.network {
                crate::recipe::Network::None => Network::None,
                crate::recipe::Network::Bridge => Network::Bridge,
            },
            profiles: recipe.profiles.keys().copied().collect(),
            scenarios: validated.declared_scenarios(),
            artifacts: InspectArtifacts {
                base: recipe.artifacts.base.as_deref().map(path_preview),
                target: path_preview(&recipe.artifacts.target),
            },
            lifetime: Lifetime::ServerProcess,
            max_run_seconds: UInt::new(max_run_seconds)
                .map_err(|_| error(ErrorCode::InvalidArguments))?,
        }));
    }
    let mut cursor = CursorRecord::recipe(&reply.instance, args).map_err(error)?;
    let revision =
        Revision::parse(&validated.digest).map_err(|_| error(ErrorCode::InternalError))?;
    let records = inspection_records::records(&validated);
    let data = match &args.selector {
        InspectSelector::Catalog {
            collection,
            profile,
        } => {
            let selected: Vec<_> = records
                .iter()
                .filter(|r| {
                    r.collection() == *collection && profile.is_none_or(|p| r.profile() == Some(p))
                })
                .collect();
            let total = selected.len() as u64;
            if let Some(c) = decoded {
                c.validate(&cursor, total).map_err(error)?;
                cursor = c;
            }
            let start = usize::try_from(cursor.next_position)
                .map_err(|_| error(ErrorCode::InvalidCursor))?;
            let summaries = selected
                .iter()
                .skip(start)
                .take(usize::from(cursor.requested_limit))
                .map(|r| r.summary())
                .collect::<Result<Vec<_>, _>>()?;
            let more = start + summaries.len() < selected.len();
            fit_page(
                reply,
                PageCandidate {
                    data: ToolData::RecipeInspect(InspectData::Catalog(CatalogPage {
                        kind: CatalogKind::Catalog,
                        revision,
                        records: summaries,
                        next_cursor: None,
                    })),
                    cursor,
                    more_after_candidate: more,
                },
            )
        }
        InspectSelector::Record { record_id } => {
            let record = records
                .iter()
                .find(|r| r.id() == record_id.as_str())
                .ok_or_else(|| error(ErrorCode::UnknownRecord))?;
            let serialize = |writer: &mut RangeWriter| {
                serde_json::to_writer(writer, record)
                    .map_err(|e| QueryError::new(ErrorCode::InternalError, e.into()))
            };
            let mut counter = RangeWriter::new(0, 0);
            serialize(&mut counter)?;
            let total = counter.total_bytes();
            if let Some(c) = decoded {
                c.validate(&cursor, total).map_err(error)?;
                cursor = c;
            }
            let mut writer =
                RangeWriter::new(cursor.next_position, usize::from(cursor.requested_limit));
            serialize(&mut writer)?;
            let more = cursor.next_position + (writer.bytes().len() as u64) < total;
            let page = RecordPage {
                kind: RecordSelectorKind::Record,
                revision,
                record_id: record_id.clone(),
                record_kind: record.kind(),
                encoding: Encoding::JsonUtf8,
                offset: UInt::new(cursor.next_position)
                    .map_err(|_| error(ErrorCode::InvalidCursor))?,
                bytes: writer.bytes().to_vec(),
                total_bytes: UInt::new(total).map_err(|_| error(ErrorCode::ResponseTooLarge))?,
                next_cursor: None,
            };
            fit_page(
                reply,
                PageCandidate {
                    data: ToolData::RecipeInspect(InspectData::Record(page)),
                    cursor,
                    more_after_candidate: more,
                },
            )
        }
        InspectSelector::Overview => unreachable!(),
    }
    .map_err(|failure| error(failure.error.code))?;
    match data {
        ToolData::RecipeInspect(value) => Ok(value),
        _ => Err(error(ErrorCode::InternalError)),
    }
}
#[cfg(test)]
#[path = "inspection_tests.rs"]
mod tests;
