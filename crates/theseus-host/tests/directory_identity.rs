use std::io;
use theseus_host::fs::{HostDir, RootPolicy};

#[test]
fn independently_opened_directories_compare_as_the_same_object() -> io::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().canonicalize()?.join("root");
    let first = HostDir::create_private(&path)?;
    let second = HostDir::open(&path, RootPolicy::AllComponentsNoFollow)?;
    assert!(first.same_object(&second)?);
    assert!(second.same_object(&first)?);
    Ok(())
}

#[test]
fn different_directories_compare_as_different_objects() -> io::Result<()> {
    let temp = tempfile::tempdir()?;
    let first = HostDir::create_private(&temp.path().join("first"))?;
    let second = HostDir::create_private(&temp.path().join("second"))?;
    assert!(!first.same_object(&second)?);
    assert!(!second.same_object(&first)?);
    Ok(())
}

#[test]
fn retained_identity_survives_rename_and_replacement_at_the_original_path() -> io::Result<()> {
    let temp = tempfile::tempdir()?;
    let parent = temp.path().canonicalize()?;
    let path = parent.join("root");
    let moved = parent.join("moved");
    let retained = HostDir::create_private(&path)?;
    std::fs::rename(&path, &moved)?;
    let replacement = HostDir::create_private(&path)?;
    let reopened = HostDir::open(&moved, RootPolicy::AllComponentsNoFollow)?;
    assert!(retained.same_object(&reopened)?);
    assert!(!retained.same_object(&replacement)?);
    Ok(())
}
