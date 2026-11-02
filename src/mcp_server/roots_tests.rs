use super::*;
use sha2::{Digest as _, Sha256};
use std::fs;
const SOURCE: &str = include_str!("../../tests/fixtures/recipes/v2_clean.toml");
pub(super) fn fixture(source: &str) -> (tempfile::TempDir, Roots) {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().canonicalize().unwrap().join("project");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("recipe.toml"), source).unwrap();
    let roots = Roots::open(
        &project,
        &temp.path().canonicalize().unwrap().join("output"),
    )
    .unwrap();
    (temp, roots)
}
#[test]
fn exact_original_newline_bytes_bind_digest() {
    let (_temp, roots) = fixture(SOURCE);
    let lf = read_recipe(&roots, "recipe.toml").unwrap();
    assert_eq!(
        lf.digest,
        format!("{:x}", Sha256::digest(SOURCE.as_bytes()))
    );
    let crlf = SOURCE.replace('\n', "\r\n");
    fs::write(roots.project_path.join("recipe.toml"), &crlf).unwrap();
    let parsed = read_recipe(&roots, "recipe.toml").unwrap();
    assert_ne!(lf.digest, parsed.digest);
    assert_eq!(
        parsed.digest,
        format!("{:x}", Sha256::digest(crlf.as_bytes()))
    );
}
#[test]
fn bounded_source_rejects_overflow_byte() {
    let (_temp, roots) = fixture(SOURCE);
    fs::write(
        roots.project_path.join("recipe.toml"),
        vec![b' '; 1_048_577],
    )
    .unwrap();
    assert_eq!(
        read_recipe(&roots, "recipe.toml").unwrap_err().code,
        ErrorCode::SourceTooLarge
    );
}
#[test]
fn portable_paths_reject_traversal_ads_and_native_prefixes() {
    let (_temp, roots) = fixture(SOURCE);
    for path in [
        "../recipe.toml",
        "/recipe.toml",
        "C:/recipe.toml",
        "recipe.toml:stream",
        "a\\recipe.toml",
        "a//b",
        "CON",
        "a/./b",
    ] {
        assert_eq!(
            read_recipe(&roots, path).unwrap_err().code,
            ErrorCode::UnsafePath,
            "{path}"
        );
    }
}
#[test]
fn renamed_and_replaced_root_rejected() {
    let (temp, roots) = fixture(SOURCE);
    fs::rename(&roots.project_path, temp.path().join("old")).unwrap();
    assert_eq!(
        read_recipe(&roots, "recipe.toml").unwrap_err().code,
        ErrorCode::RootChanged
    );
    fs::create_dir(&roots.project_path).unwrap();
    fs::write(roots.project_path.join("recipe.toml"), SOURCE).unwrap();
    assert_eq!(
        read_recipe(&roots, "recipe.toml").unwrap_err().code,
        ErrorCode::RootChanged
    );
}
#[cfg(unix)]
#[test]
fn source_and_ancestry_links_rejected() {
    use std::os::unix::fs::symlink;
    let (temp, roots) = fixture(SOURCE);
    symlink("recipe.toml", roots.project_path.join("link.toml")).unwrap();
    assert_eq!(
        read_recipe(&roots, "link.toml").unwrap_err().code,
        ErrorCode::UnsafePath
    );
    symlink(&roots.project_path, temp.path().join("alias")).unwrap();
    assert_eq!(
        Roots::open(
            &temp.path().join("alias"),
            &temp.path().join("other-output")
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::UnsafePath
    );
}

#[test]
fn retained_nested_parent_detects_rename_and_substitution() {
    let (_temp, roots) = fixture(SOURCE);
    fs::create_dir(roots.project_path.join("nested")).unwrap();
    let relative = RelativePath::parse("nested").unwrap();
    let parent = roots.project.open_child(&relative).unwrap();
    let named = roots.project_path.join("nested");
    fs::rename(&named, roots.project_path.join("old-nested")).unwrap();
    assert_eq!(
        roots
            .verify_parent(Some(&relative), &parent, &named)
            .unwrap_err()
            .code,
        ErrorCode::RootChanged
    );
    fs::create_dir(&named).unwrap();
    assert_eq!(
        roots
            .verify_parent(Some(&relative), &parent, &named)
            .unwrap_err()
            .code,
        ErrorCode::RootChanged
    );
}
#[test]
fn parser_artifact_validation_uses_nested_source_context() {
    let (_temp, roots) = fixture(SOURCE);
    fs::create_dir_all(roots.project_path.join("nested/target")).unwrap();
    fs::write(roots.project_path.join("nested/recipe.toml"), SOURCE).unwrap();
    let parsed = read_recipe(&roots, "nested/recipe.toml").unwrap();
    assert_eq!(parsed.project_root, roots.project_path.join("nested"));
    assert_eq!(
        parsed.target_artifact,
        roots.project_path.join("nested/target")
    );
    fs::remove_dir(roots.project_path.join("nested/target")).unwrap();
    assert_eq!(
        read_recipe(&roots, "nested/recipe.toml").unwrap_err().code,
        ErrorCode::RecipeInvalid
    );
}
#[cfg(unix)]
#[test]
fn startup_preserves_existing_directory_modes() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, roots) = fixture(SOURCE);
    let output = temp.path().canonicalize().unwrap().join("output");
    fs::set_permissions(&output, fs::Permissions::from_mode(0o751)).unwrap();
    Roots::open(&roots.project_path, &output).unwrap();
    assert_eq!(
        fs::metadata(output).unwrap().permissions().mode() & 0o777,
        0o751
    );
}

#[cfg(windows)]
#[test]
fn junction_source_parent_and_startup_alias_are_unsafe() {
    let (temp, roots) = fixture(SOURCE);
    let link = roots.project_path.join("junction");
    let target = roots.project_path.join("target");
    fs::write(target.join("recipe.toml"), SOURCE).unwrap();
    let status = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&link)
        .arg(&target)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        read_recipe(&roots, "junction/recipe.toml")
            .unwrap_err()
            .code,
        ErrorCode::UnsafePath
    );
    assert_eq!(
        Roots::open(&link, &temp.path().join("other-output"))
            .err()
            .unwrap()
            .code,
        ErrorCode::UnsafePath
    );
}

#[test]
fn exact_one_mebibyte_source_remains_valid() {
    let (_temp, roots) = fixture(SOURCE);
    let mut bytes = SOURCE.as_bytes().to_vec();
    bytes.resize(1_048_576, b' ');
    fs::write(roots.project_path.join("recipe.toml"), &bytes).unwrap();
    assert_eq!(
        read_recipe(&roots, "recipe.toml").unwrap().digest,
        format!("{:x}", Sha256::digest(&bytes))
    );
}
#[cfg(unix)]
#[test]
fn non_utf8_native_root_retains_identity_without_display_conversion() {
    use std::os::unix::ffi::OsStringExt;
    let temp = tempfile::tempdir().unwrap();
    let native = std::ffi::OsString::from_vec(b"project-\xff".to_vec());
    let path = temp.path().canonicalize().unwrap().join(native);
    fs::create_dir_all(path.join("target")).unwrap();
    fs::write(path.join("recipe.toml"), SOURCE).unwrap();
    let roots = Roots::open(&path, &temp.path().canonicalize().unwrap().join("output")).unwrap();
    assert_eq!(
        read_recipe(&roots, "recipe.toml").unwrap().project_root,
        path
    );
    fs::rename(&path, temp.path().join("renamed")).unwrap();
    assert_eq!(
        read_recipe(&roots, "recipe.toml").unwrap_err().code,
        ErrorCode::RootChanged
    );
}
