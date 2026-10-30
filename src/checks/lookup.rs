use crate::{
    model::FileKind,
    recipe::validate::contained,
    snapshot::{FileEntry, ObservationStatus, Snapshot},
};

/// The outer Result distinguishes unobserved evidence from an observed missing path.
pub(super) fn observed<'a>(
    path: &'a str,
    snapshots: &'a [Snapshot],
) -> Result<Option<&'a FileEntry>, String> {
    let snapshot = observed_snapshot(path, snapshots)?;
    Ok(if snapshot.completeness == ObservationStatus::Missing {
        None
    } else {
        snapshot.entries.get(path)
    })
}

/// Resolve unique coverage and reject incomplete or no-follow ancestor evidence.
pub(super) fn observed_snapshot<'a>(
    path: &'a str,
    snapshots: &'a [Snapshot],
) -> Result<&'a Snapshot, String> {
    observed_snapshot_reason(path, snapshots).map_err(|error| error.0.concat())
}
/// Borrowed error pieces let comparison accounting precede diagnostic allocation.
pub(crate) struct ObservationError<'a>(pub [&'a str; 5]);
fn observed_snapshot_reason<'a>(
    path: &'a str,
    snapshots: &'a [Snapshot],
) -> Result<&'a Snapshot, ObservationError<'a>> {
    let mut matching = snapshots.iter().filter(|s| contained(path, &s.root));
    let snapshot = matching.next().ok_or(ObservationError([
        "no observed root covers ",
        path,
        "",
        "",
        "",
    ]))?;
    if matching.next().is_some() {
        return Err(ObservationError([
            "ambiguous overlapping observations cover ",
            path,
            "",
            "",
            "",
        ]));
    }
    match &snapshot.completeness {
        ObservationStatus::Complete => {
            // A no-follow snapshot proves the link entry, not anything reached
            // through it. The same uncertainty applies to non-directory parents.
            // Inspect proper ancestors only, so checking the link itself stays valid.
            for (index, _) in path.match_indices('/').skip(1) {
                let ancestor = &path[..index];
                if snapshot
                    .entries
                    .get(ancestor)
                    .is_some_and(|entry| entry.kind != FileKind::Directory)
                {
                    return Err(ObservationError([
                        "cannot observe ",
                        path,
                        " through non-directory ancestor ",
                        ancestor,
                        "",
                    ]));
                }
            }
            Ok(snapshot)
        }
        ObservationStatus::Missing => Ok(snapshot),
        ObservationStatus::Incomplete { reason } => Err(ObservationError([
            "observation of ",
            &snapshot.root,
            " is incomplete: ",
            reason,
            "",
        ])),
    }
}

/// A recursive scope additionally rejects observations nested beneath it.
pub(crate) fn observed_scope<'a>(
    path: &'a str,
    snapshots: &'a [Snapshot],
) -> Result<&'a Snapshot, String> {
    observed_scope_reason(path, snapshots).map_err(|error| error.0.concat())
}
pub(crate) fn observed_scope_reason<'a>(
    path: &'a str,
    snapshots: &'a [Snapshot],
) -> Result<&'a Snapshot, ObservationError<'a>> {
    let snapshot = observed_snapshot_reason(path, snapshots)?;
    // Exact lookup detects roots covering the path. A recursive scope must also
    // reject extra observations nested beneath it, including incomplete ones.
    if snapshots
        .iter()
        .any(|other| other.root != snapshot.root && contained(&other.root, path))
    {
        return Err(ObservationError([
            "ambiguous overlapping observations cover stable scope ",
            path,
            "",
            "",
            "",
        ]));
    }
    Ok(snapshot)
}
