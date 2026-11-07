//! Original fixture SUT publication of real, inert installed desktop entries.
use super::*;
use std::path::PathBuf;
pub fn metadata(
    definition: &v2::OwnedDesktopEntryDefinition,
    x: &v2::DesktopExecutable,
) -> Result<Vec<u8>> {
    match definition {
        v2::OwnedDesktopEntryDefinition::MacosBundle {
            bundle_identifier,
            executable_leaf,
            ..
        } => {
            ensure!(
                [bundle_identifier, executable_leaf]
                    .iter()
                    .all(|s| !s.is_empty()
                        && s.len() <= 255
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))),
                "closed fixture plist strings"
            );
            Ok(format!("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>{bundle_identifier}</string><key>CFBundleExecutable</key><string>{executable_leaf}</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>").into_bytes())
        }
        v2::OwnedDesktopEntryDefinition::WindowsShortcut => {
            #[cfg(feature = "native-integration-test-support")]
            {
                Ok(theseus_host::desktop::windows::fixture::shortcut_bytes(
                    &x.root,
                    &x.relative_path,
                    &x.arguments[1..],
                )?)
            }
            #[cfg(not(feature = "native-integration-test-support"))]
            {
                let _ = x;
                anyhow::bail!("native entry fixture encoder feature required")
            }
        }
    }
}
pub struct Layout {
    root: PathBuf,
    entry: PathBuf,
    executable: PathBuf,
    app: Option<PathBuf>,
}
impl Layout {
    pub fn new(manifest: &v2::IntegrationManifest) -> Result<Option<Self>> {
        let Some(definition) = &manifest.desktop_owned_entry else {
            return Ok(None);
        };
        let desktop = manifest.desktop.as_ref().context("entry declaration")?;
        let x = manifest
            .desktop_executable
            .as_ref()
            .context("entry executable")?;
        let root = PathBuf::from(&desktop.entry_root);
        Ok(Some(Self {
            entry: root.join(definition.entry_relative_path(desktop)),
            executable: root.join(&x.relative_path),
            app: match definition {
                v2::OwnedDesktopEntryDefinition::MacosBundle { app_leaf, .. } => {
                    Some(root.join(app_leaf))
                }
                _ => None,
            },
            root,
        }))
    }
    pub fn entry(&self) -> &Path {
        &self.entry
    }
    fn exact_children(path: &Path, names: &[&str]) -> Result<()> {
        let _root = HostDir::open(path, RootPolicy::AllComponentsNoFollow)?;
        let actual = std::fs::read_dir(path)?
            .map(|r| Ok(r?.file_name()))
            .collect::<std::io::Result<std::collections::BTreeSet<_>>>()?;
        let expected = names
            .iter()
            .map(std::ffi::OsString::from)
            .collect::<std::collections::BTreeSet<_>>();
        ensure!(
            actual == expected,
            "foreign fixture entry directory cannot be adopted"
        );
        Ok(())
    }
    pub fn check_before(&self, original: Option<&[u8]>, allowed: &[Vec<u8>]) -> Result<()> {
        let Some(original) = original else {
            return Self::exact_children(&self.root, &[]);
        };
        if let Some(app) = &self.app {
            Self::exact_children(&self.root, &[app.file_name().unwrap().to_str().unwrap()])?;
            Self::exact_children(app, &["Contents"])?;
            Self::exact_children(&app.join("Contents"), &["Info.plist", "MacOS"])?;
            Self::exact_children(
                &app.join("Contents/MacOS"),
                &[self.executable.file_name().unwrap().to_str().unwrap()],
            )?;
        } else {
            Self::exact_children(
                &self.root,
                &[
                    self.entry.file_name().unwrap().to_str().unwrap(),
                    self.executable.file_name().unwrap().to_str().unwrap(),
                ],
            )?;
        }
        let read = |path: &Path| -> Result<Option<Vec<u8>>> {
            retained_bytes(
                &HostDir::open(path.parent().unwrap(), RootPolicy::AllComponentsNoFollow)?,
                path.file_name().unwrap().to_str().context("fixture leaf")?,
            )
        };
        ensure!(
            read(&self.executable)?.as_deref() == Some(original),
            "second executable differs from original core reader"
        );
        ensure!(
            read(&self.entry)?.is_some_and(|b| allowed.contains(&b)),
            "foreign native metadata cannot be replaced"
        );
        Ok(())
    }
    fn publish_metadata(&self, bytes: &[u8]) -> Result<()> {
        let parent = HostDir::open(
            self.entry.parent().unwrap(),
            RootPolicy::AllComponentsNoFollow,
        )?;
        let name = self.entry.file_name().unwrap();
        if retained_bytes(&parent, name.to_str().unwrap())?.as_deref() != Some(bytes) {
            parent.publish(name, &mut |file| file.write_all(bytes))?;
        }
        Ok(())
    }
    pub fn install(&self, image: &[u8], bytes: &[u8], fresh: bool) -> Result<()> {
        if fresh && let Some(app) = &self.app {
            for path in [
                app.clone(),
                app.join("Contents"),
                app.join("Contents/MacOS"),
            ] {
                HostDir::create_private(&path)?;
            }
        }
        let parent = HostDir::open(
            self.executable.parent().unwrap(),
            RootPolicy::AllComponentsNoFollow,
        )?;
        parent.publish(self.executable.file_name().unwrap(), &mut |file| {
            file.write_all(image)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(0o500))?;
            }
            Ok(())
        })?;
        self.publish_metadata(bytes)
    }
    pub fn configure(&self, bytes: &[u8]) -> Result<()> {
        self.publish_metadata(bytes)
    }
    pub fn uninstall(&self, images: &[Vec<u8>], entries: &[Vec<u8>]) -> Result<()> {
        remove_exact(&self.entry, entries)?;
        remove_exact(&self.executable, images)?;
        if let Some(app) = &self.app {
            for path in [
                app.join("Contents/MacOS"),
                app.join("Contents"),
                app.clone(),
            ] {
                std::fs::remove_dir(path)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_owned_entry_sut_keeps_configure_identity_and_rejects_foreign_layout() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?.join("entries");
        HostDir::create_private(&root)?;
        let app = root.join("Fixture.app");
        let layout = Layout {
            root: root.clone(),
            entry: app.join("Contents/Info.plist"),
            executable: app.join("Contents/MacOS/reader"),
            app: Some(app.clone()),
        };
        let image = b"pinned original executable".to_vec();
        let bytes = b"exact metadata".to_vec();
        layout.check_before(None, std::slice::from_ref(&bytes))?;
        layout.install(&image, &bytes, true)?;
        let parent = HostDir::open(
            layout.entry.parent().unwrap(),
            RootPolicy::AllComponentsNoFollow,
        )?;
        let LeafOpenResult::Opened(original) =
            parent.open_leaf_classified(std::ffi::OsStr::new("Info.plist"))
        else {
            anyhow::bail!("original metadata owner")
        };
        let identity = fingerprint(&original)?;
        layout.check_before(Some(&image), std::slice::from_ref(&bytes))?;
        layout.configure(&bytes)?;
        let LeafOpenResult::Opened(current) =
            parent.open_leaf_classified(std::ffi::OsStr::new("Info.plist"))
        else {
            anyhow::bail!("current metadata owner")
        };
        assert_eq!(identity, fingerprint(&current)?);
        std::fs::write(app.join("foreign"), b"foreign")?;
        assert!(
            layout
                .check_before(Some(&image), std::slice::from_ref(&bytes))
                .is_err()
        );
        std::fs::remove_file(app.join("foreign"))?;
        drop(current);
        drop(original);
        drop(parent);
        layout.uninstall(std::slice::from_ref(&image), std::slice::from_ref(&bytes))?;
        assert!(std::fs::read_dir(&root)?.next().is_none());
        Ok(())
    }
}
