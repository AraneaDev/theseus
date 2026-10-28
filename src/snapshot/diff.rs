use super::*;
use std::collections::BTreeSet;
/// Deterministic observed differences. Callers must retain each snapshot's completeness.
pub fn diff(before: &Snapshot, after: &Snapshot) -> Vec<FileChange> {
    before
        .entries
        .keys()
        .chain(after.entries.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|path| {
            let old = before.entries.get(path);
            let new = after.entries.get(path);
            if old == new {
                return None;
            }
            Some(FileChange {
                path: path.clone(),
                kind: match (old, new) {
                    (None, Some(_)) => ChangeKind::Added,
                    (Some(_), None) => ChangeKind::Removed,
                    _ => ChangeKind::Modified,
                },
                before: old.cloned(),
                after: new.cloned(),
            })
        })
        .collect()
}
