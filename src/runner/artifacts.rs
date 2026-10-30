use crate::{
    engine::CancellationToken,
    model::{ArtifactDigest, ArtifactFileDigest},
    recipe::ValidatedRecipe,
};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use theseus_host::fs::{HostDir, RelativePath, RootPolicy};

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
    let root = HostDir::open(project, RootPolicy::LeafNoFollow)?;
    let relative_source = posix_relative(resolved.strip_prefix(project)?)?;
    let input = if relative_source.is_empty() {
        None
    } else {
        root.open_file(&RelativePath::parse(&relative_source)?).ok()
    };
    match input {
        None => {
            ensure!(
                ancestors.insert(resolved.clone()),
                "artifact directory cycle"
            );
            fs::create_dir(target)?;
            #[cfg(unix)]
            fs::set_permissions(target, fs::Permissions::from_mode(0o755))?;
            let directory = if relative_source.is_empty() {
                root
            } else {
                root.open_child(&RelativePath::parse(&relative_source)?)?
            };
            for name in directory.entries()? {
                copy(
                    &resolved.join(name.clone()),
                    &target.join(name.clone()),
                    &relative.join(name.clone()),
                    project,
                    ancestors,
                    files,
                    cancel,
                )?;
            }
            ancestors.remove(&resolved);
        }
        Some(mut input) => {
            #[cfg(unix)]
            let metadata = input.metadata()?;
            let parent = HostDir::open(
                target.parent().context("artifact target parent")?,
                RootPolicy::LeafNoFollow,
            )?;
            let name = target
                .file_name()
                .and_then(|s| s.to_str())
                .context("artifact target must be UTF-8")?;
            let mut output = parent.create_file(&RelativePath::parse(name)?)?;
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
            #[cfg(unix)]
            output.set_permissions(fs::Permissions::from_mode(
                (metadata.permissions().mode() & 0o111) | 0o444,
            ))?;
            files.push(ArtifactFileDigest {
                path: if relative.as_os_str().is_empty() {
                    ".".into()
                } else {
                    posix_relative(relative)?
                },
                sha256: format!("{:x}", hash.finalize()),
                size,
            });
        }
    }
    Ok(())
}

// Stored manifest paths are POSIX references, independent of host separators.
fn posix_relative(path: &Path) -> Result<String> {
    path.components()
        .map(|part| {
            let std::path::Component::Normal(name) = part else {
                anyhow::bail!("invalid canonical artifact component")
            };
            name.to_str().context("artifact path must be UTF-8")
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join("/"))
}

/// Windows-host inputs carry bytes, with guest modes declared only on owned roots.
#[cfg(any(windows, test))]
pub(super) fn windows_input_normalization() -> &'static str {
    r#"set -eu
command -v find >/dev/null || { echo 'required tool missing: find' >&2; exit 1; }
for input in /inputs/base /inputs/target; do
  if test -e "$input"; then
    find "$input" -type d -exec chmod 0755 {} +
    find "$input" -type f -exec chmod 0444 {} +
  fi
done
"#
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;
    #[cfg(unix)]
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
    #[cfg(unix)]
    #[test]
    fn descriptor_open_refuses_a_symlink_ancestor_even_after_validation() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let outside = temp.path().join("outside");
        fs::create_dir(&project).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret"), b"private").unwrap();
        std::os::unix::fs::symlink(&outside, project.join("replaced")).unwrap();
        let root = HostDir::open(&project, RootPolicy::LeafNoFollow).unwrap();
        assert!(
            root.open_file(&RelativePath::parse("replaced/secret").unwrap())
                .is_err()
        );
    }
}

#[cfg(test)]
mod portable_tests {
    use super::*;
    #[test]
    fn file_and_directory_inputs_keep_bytes_sorted_posix_names_and_archive_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir_all(project.join("input/nested")).unwrap();
        let archive = b"\x7fELF\r\n\0opaque tar bytes\xff";
        fs::write(project.join("bundle.tar"), archive).unwrap();
        fs::write(project.join("input/z.exe"), b"data only").unwrap();
        fs::write(project.join("input/nested/a.sh"), b"#!/bin/sh\n").unwrap();
        let mut recipe =
            crate::recipe::load(Path::new("tests/fixtures/lifecycle/recipe.toml")).unwrap();
        recipe.project_root = project.canonicalize().unwrap();
        recipe.base_artifact = Some(project.join("bundle.tar"));
        recipe.target_artifact = project.join("input");
        let inputs = materialize(&recipe, &CancellationToken::default()).unwrap();
        assert_eq!(
            fs::read(inputs.directory.path().join("base")).unwrap(),
            archive
        );
        assert_eq!(inputs.manifests[0].files[0].path, ".");
        assert_eq!(
            inputs.manifests[1]
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            ["nested/a.sh", "z.exe"]
        );
        assert_eq!(
            fs::read(inputs.directory.path().join("target/z.exe")).unwrap(),
            b"data only"
        );
    }
    #[cfg(unix)]
    #[test]
    fn unix_staging_preserves_only_source_execute_bits_and_windows_guest_script_sets_data_modes() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir_all(project.join("input/subdir")).unwrap();
        fs::write(project.join("input/data.exe"), b"plain bytes").unwrap();
        fs::write(project.join("input/executable.sh"), b"#!/bin/sh\n").unwrap();
        fs::set_permissions(
            project.join("input/data.exe"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::set_permissions(
            project.join("input/executable.sh"),
            fs::Permissions::from_mode(0o710),
        )
        .unwrap();
        let mut recipe =
            crate::recipe::load(Path::new("tests/fixtures/lifecycle/recipe.toml")).unwrap();
        recipe.project_root = project.canonicalize().unwrap();
        recipe.base_artifact = None;
        recipe.target_artifact = project.join("input");
        let inputs = materialize(&recipe, &CancellationToken::default()).unwrap();
        let target = inputs.directory.path().join("target");
        assert_eq!(
            fs::metadata(target.join("data.exe"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o444
        );
        assert_eq!(
            fs::metadata(target.join("executable.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o554
        );
        // Execute the Windows-controller guest policy only on this owned fixture.
        // Supplied fixture bytes themselves are never executed.
        let script = windows_input_normalization()
            .replace("/inputs", inputs.directory.path().to_str().unwrap());
        let result = std::process::Command::new("sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        for name in ["data.exe", "executable.sh"] {
            assert_eq!(
                fs::metadata(target.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o444
            );
        }
        assert_eq!(
            fs::metadata(target.join("subdir"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(fs::read(target.join("data.exe")).unwrap(), b"plain bytes");
    }
    #[test]
    fn windows_guest_policy_declares_data_modes_without_extension_inference() {
        let script = windows_input_normalization();
        assert!(script.contains("/inputs/base /inputs/target"));
        assert!(script.contains("-type d -exec chmod 0755"));
        assert!(script.contains("-type f -exec chmod 0444"));
        for extension in [".exe", ".sh", ".bat", ".cmd"] {
            assert!(!script.contains(extension));
        }
    }
}
