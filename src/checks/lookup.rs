use crate::{
    model::FileKind,
    recipe::validate::contained,
    snapshot::{FileEntry, ObservationStatus, Snapshot},
};

/// The outer Result distinguishes unobserved evidence from an observed missing path.
pub(super) fn observed<'a>(
    path: &str,
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
    path: &str,
    snapshots: &'a [Snapshot],
) -> Result<&'a Snapshot, String> {
    let mut matching = snapshots.iter().filter(|s| contained(path, &s.root));
    let snapshot = matching
        .next()
        .ok_or_else(|| format!("no observed root covers {path}"))?;
    if matching.next().is_some() {
        return Err(format!("ambiguous overlapping observations cover {path}"));
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
                    return Err(format!(
                        "cannot observe {path} through non-directory ancestor {ancestor}"
                    ));
                }
            }
            Ok(snapshot)
        }
        ObservationStatus::Missing => Ok(snapshot),
        ObservationStatus::Incomplete { reason } => Err(format!(
            "observation of {} is incomplete: {reason}",
            snapshot.root
        )),
    }
}
