use std::io::{self, Write};
use theseus_host::fs::{HostDir, RelativePath, RootPolicy, fingerprint, read_exact_at};

#[test]
fn exclusive_creation_and_offset_read() -> io::Result<()> {
    let parent = tempfile::tempdir()?;
    let root = HostDir::create_private(&parent.path().join("run"))?;
    let name = RelativePath::parse("logs/out")?;
    let mut file = root.create_file(&name)?;
    file.write_all(b"abcdef")?;
    assert!(root.create_file(&name).is_err());
    let mut page = [0; 3];
    read_exact_at(&root.open_file(&name)?, &mut page, 2)?;
    assert_eq!(&page, b"cde");
    assert_eq!(
        read_exact_at(&file, &mut page, 5).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    Ok(())
}

#[test]
fn directories_are_exclusive_sorted_and_regular_files_required() -> io::Result<()> {
    let parent = tempfile::tempdir()?;
    let path = parent.path().join("missing/run");
    let root = HostDir::create_private(&path)?;
    assert!(HostDir::create_private(&path).is_err());
    root.create_child("z".as_ref())?;
    root.create_file(&RelativePath::parse("a")?)?;
    assert!(root.create_child("z".as_ref()).is_err());
    assert!(root.create_child("a/b".as_ref()).is_err());
    assert!(root.create_child("..".as_ref()).is_err());
    assert!(root.open_file(&RelativePath::parse("z")?).is_err());
    assert!(root.open_child(&RelativePath::parse("a")?).is_err());
    assert_eq!(
        root.entries()?,
        vec![std::ffi::OsString::from("a"), "z".into()]
    );
    assert_eq!(root.entries()?.len(), 2);
    Ok(())
}

#[test]
fn retained_root_and_file_survive_replacement() -> io::Result<()> {
    let parent = tempfile::tempdir()?;
    let path = parent.path().join("run");
    let root = HostDir::create_private(&path)?;
    let name = RelativePath::parse("file")?;
    let mut old = root.create_file(&name)?;
    old.write_all(b"same")?;
    let times = std::fs::FileTimes::new().set_modified(old.metadata()?.modified()?);
    let original = fingerprint(&old)?;
    std::fs::rename(path.join("file"), path.join("old"))
        .map_err(|e| io::Error::new(e.kind(), format!("rename retained file: {e}")))?;
    let mut replacement = root.create_file(&name)?;
    replacement.write_all(b"same")?;
    replacement
        .set_times(times)
        .map_err(|e| io::Error::new(e.kind(), format!("set replacement time: {e}")))?;
    assert_ne!(fingerprint(&old)?, fingerprint(&replacement)?);
    assert_eq!(fingerprint(&old)?.len, original.len);
    std::fs::rename(&path, parent.path().join("moved"))
        .map_err(|e| io::Error::new(e.kind(), format!("rename retained root: {e}")))?;
    let new_root = HostDir::create_private(&path)?;
    new_root.create_file(&RelativePath::parse("other")?)?;
    assert_eq!(root.entries()?.len(), 2);
    assert_eq!(new_root.entries()?.len(), 1);
    let mut bytes = [0; 4];
    read_exact_at(&root.open_file(&name)?, &mut bytes, 0)?;
    assert_eq!(&bytes, b"same");
    Ok(())
}

#[cfg(unix)]
#[test]
fn links_are_rejected_and_ancestor_policy_is_explicit() -> io::Result<()> {
    use std::os::unix::fs::symlink;
    // Resolve the harness temp ancestor (Darwin /var is an alias), not a production root.
    let parent = tempfile::tempdir()?;
    let parent_path = parent.path().canonicalize()?;
    let root_path = parent_path.join("run");
    let root = HostDir::create_private(&root_path)?;
    root.create_child("dir".as_ref())?;
    root.create_file(&RelativePath::parse("dir/data")?)?;
    symlink(&root_path, parent_path.join("alias"))?;
    assert!(HostDir::open(&parent_path.join("alias"), RootPolicy::LeafNoFollow).is_err());
    HostDir::open(&parent_path.join("alias/dir"), RootPolicy::LeafNoFollow)?;
    assert!(
        HostDir::open(
            &parent_path.join("alias/dir"),
            RootPolicy::AllComponentsNoFollow
        )
        .is_err()
    );
    symlink("dir/data", root_path.join("link"))?;
    assert!(root.open_file(&RelativePath::parse("link")?).is_err());
    std::fs::rename(root_path.join("dir"), root_path.join("original"))?;
    symlink(parent_path.clone(), root_path.join("dir"))?;
    assert!(
        root.create_file(&RelativePath::parse("dir/escape")?)
            .is_err()
    );
    assert!(!parent_path.join("escape").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn unix_private_modes_and_fifo_rejection() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let parent = tempfile::tempdir()?;
    let root = HostDir::create_private(&parent.path().join("run"))?;
    let file = root.create_file(&RelativePath::parse("dir/data")?)?;
    assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::metadata(parent.path().join("run/dir"))?
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    // Darwin has no mknodat binding; both Unix runners provide mkfifo.
    let status = std::process::Command::new("mkfifo")
        .arg(parent.path().join("run/fifo"))
        .status()?;
    assert!(status.success(), "could not create the real FIFO fixture");
    assert!(root.open_file(&RelativePath::parse("fifo")?).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn root_leaf_alias_cannot_hide_behind_trailing_separators() -> io::Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::create_dir(temp.path().join("real"))?;
    std::os::unix::fs::symlink("real", temp.path().join("alias"))?;
    for spelling in ["alias/", "alias/."] {
        assert!(HostDir::open(&temp.path().join(spelling), RootPolicy::LeafNoFollow).is_err());
    }
    Ok(())
}
#[cfg(unix)]
#[test]
fn private_creation_accepts_existing_parent_alias() -> io::Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::create_dir(temp.path().join("real"))?;
    std::os::unix::fs::symlink("real", temp.path().join("alias"))?;
    HostDir::create_private(&temp.path().join("alias/new"))?;
    assert!(temp.path().join("real/new").is_dir());
    Ok(())
}

#[test]
fn strict_root_rejects_parent_components() -> io::Result<()> {
    let temp = tempfile::tempdir()?;
    let trusted = temp.path().canonicalize()?;
    let root = HostDir::create_private(&trusted.join("run"))?;
    root.create_child("child".as_ref())?;
    // PathBuf::join normalizes '..' for Windows verbatim paths returned by
    // canonicalize. Append native text to actually deliver the unsafe component.
    let mut raw = trusted.join("run/child").into_os_string();
    raw.push(std::path::MAIN_SEPARATOR_STR);
    raw.push("..");
    let unsafe_path = std::path::PathBuf::from(raw);
    assert!(
        unsafe_path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    );
    assert!(HostDir::open(&unsafe_path, RootPolicy::AllComponentsNoFollow).is_err());
    Ok(())
}

#[test]
fn enumeration_handles_multiple_batches_and_concurrent_readers() -> io::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = HostDir::create_private(&temp.path().join("run"))?;
    let mut expected = Vec::new();
    // Exceed a native 64KiB directory buffer, including variable UTF-16 names.
    for index in 0..360 {
        let name = format!("{index:04}-é-{}", "x".repeat(96));
        root.create_file(&RelativePath::parse(&name)?)?;
        expected.push(std::ffi::OsString::from(name));
    }
    std::thread::scope(|scope| {
        let a = scope.spawn(|| root.entries());
        let b = scope.spawn(|| root.entries());
        assert_eq!(a.join().unwrap()?, expected);
        assert_eq!(b.join().unwrap()?, expected);
        Ok::<_, io::Error>(())
    })?;
    Ok(())
}
