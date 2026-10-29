use crate::{
    engine::CancellationToken,
    model::{ArtifactDigest, ArtifactFileDigest},
    recipe::ValidatedRecipe,
};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub(super) struct Inputs {
    pub directory: tempfile::TempDir,
    pub manifests: Vec<ArtifactDigest>,
}
pub(super) fn materialize(recipe: &ValidatedRecipe, cancel: &CancellationToken) -> Result<Inputs> {
    let directory = tempfile::Builder::new()
        .prefix("theseus-inputs-")
        .tempdir()?;
    let mut manifests = vec![];
    for (name, source) in [
        ("base", recipe.base_artifact.as_ref()),
        ("target", Some(&recipe.target_artifact)),
    ] {
        if let Some(source) = source {
            let mut files = vec![];
            copy(
                source,
                &directory.path().join(name),
                Path::new(""),
                &recipe.project_root,
                &mut BTreeSet::new(),
                &mut files,
                cancel,
            )?;
            files.sort_by(|a, b| a.path.cmp(&b.path));
            let mut digest = Sha256::new();
            for file in &files {
                digest.update(file.path.as_bytes());
                digest.update([0]);
                digest.update(file.size.to_le_bytes());
                digest.update(file.sha256.as_bytes());
            }
            manifests.push(ArtifactDigest {
                name: name.into(),
                sha256: format!("{:x}", digest.finalize()),
                files,
            });
        }
    }
    Ok(Inputs {
        directory,
        manifests,
    })
}
fn copy(
    source: &Path,
    target: &Path,
    relative: &Path,
    project: &Path,
    ancestors: &mut BTreeSet<PathBuf>,
    files: &mut Vec<ArtifactFileDigest>,
    cancel: &CancellationToken,
) -> Result<()> {
    ensure!(!cancel.is_cancelled(), "artifact materialization cancelled");
    let resolved = source
        .canonicalize()
        .context("resolve artifact while materializing")?;
    ensure!(
        resolved.starts_with(project),
        "artifact escaped project during materialization"
    );
    let metadata = fs::metadata(&resolved)?;
    if metadata.is_dir() {
        ensure!(
            ancestors.insert(resolved.clone()),
            "artifact directory cycle"
        );
        fs::create_dir(target)?;
        fs::set_permissions(target, fs::Permissions::from_mode(0o755))?;
        let directory = open_confined(project, &resolved)?;
        use std::os::fd::AsRawFd;
        let mut entries: Vec<_> = fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?
            .collect::<std::io::Result<_>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            copy(
                &resolved.join(entry.file_name()),
                &target.join(entry.file_name()),
                &relative.join(entry.file_name()),
                project,
                ancestors,
                files,
                cancel,
            )?;
        }
        ancestors.remove(&resolved);
    } else {
        ensure!(metadata.is_file(), "artifact has unsupported special file");
        let mut input = open_confined(project, &resolved)?;
        ensure!(
            input.metadata()?.is_file(),
            "artifact is not a regular file"
        );
        let mut output = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(target)?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut buffer = [0; 32768];
        loop {
            ensure!(!cancel.is_cancelled(), "artifact copy cancelled");
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count])?;
            hash.update(&buffer[..count]);
            size = size
                .checked_add(count as u64)
                .context("artifact size overflow")?;
        }
        output.sync_all()?;
        fs::set_permissions(
            target,
            fs::Permissions::from_mode((metadata.permissions().mode() & 0o111) | 0o444),
        )?;
        files.push(ArtifactFileDigest {
            path: if relative.as_os_str().is_empty() {
                ".".into()
            } else {
                relative
                    .to_str()
                    .context("artifact path must be UTF-8")?
                    .into()
            },
            sha256: format!("{:x}", hash.finalize()),
            size,
        });
    }
    Ok(())
}

// Open each canonical component relative to the project directory, closing races
// through replaced symlink ancestors. NONBLOCK also prevents a raced FIFO hang.
fn open_confined(project: &Path, resolved: &Path) -> Result<fs::File> {
    use rustix::fs::{Mode, OFlags, open, openat};
    let mut descriptor = open(
        project,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let relative = resolved.strip_prefix(project)?;
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        let std::path::Component::Normal(name) = component else {
            anyhow::bail!("invalid canonical artifact component");
        };
        let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        if index + 1 < components.len() {
            flags |= OFlags::DIRECTORY;
        }
        descriptor = openat(&descriptor, *name, flags, Mode::empty())?;
    }
    Ok(fs::File::from(descriptor))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn materializes_contained_links_and_hashes_actual_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::create_dir(project.join("input")).unwrap();
        fs::write(project.join("data"), b"abc").unwrap();
        std::os::unix::fs::symlink("../data", project.join("input/link")).unwrap();
        let mut recipe =
            crate::recipe::load(Path::new("tests/fixtures/lifecycle/recipe.toml")).unwrap();
        recipe.project_root = project.canonicalize().unwrap();
        recipe.target_artifact = project.join("input");
        recipe.base_artifact = None;
        let inputs = materialize(&recipe, &CancellationToken::default()).unwrap();
        assert_eq!(
            fs::read(inputs.directory.path().join("target/link")).unwrap(),
            b"abc"
        );
        assert!(
            !fs::symlink_metadata(inputs.directory.path().join("target/link"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            inputs.manifests[0].files[0].sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        fs::remove_file(project.join("input/link")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", project.join("input/link")).unwrap();
        assert!(materialize(&recipe, &CancellationToken::default()).is_err());
    }
    #[test]
    fn descriptor_open_refuses_a_symlink_ancestor_even_after_validation() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let outside = temp.path().join("outside");
        fs::create_dir(&project).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret"), b"private").unwrap();
        std::os::unix::fs::symlink(&outside, project.join("replaced")).unwrap();
        assert!(open_confined(&project, &project.join("replaced/secret")).is_err());
    }
}
