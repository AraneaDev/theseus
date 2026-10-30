use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use theseus::{
    comparison,
    preview::{DocumentKind, DraftDocument, MAX_DRAFT_BYTES, PreviewSource, ValidatedDocument},
    recipe,
};

const RECIPE: &str = include_str!("fixtures/recipes/v2_clean.toml");
const SUITE: &str = "schema_version=1\nname='methods'\nbaseline='a'\nprofiles=['user']\nscenarios=['clean']\nstages=['configured']\n[[variants]]\nid='a'\nrecipe='recipes/a.toml'\n[[variants]]\nid='b'\nrecipe='recipes/b.toml'\n[[scopes]]\nid='application'\nprofile='user'\npath='/home//theseus/./'\n";

fn tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("recipes/target")).unwrap();
    fs::create_dir(dir.path().join("target")).unwrap();
    fs::write(dir.path().join("recipe.toml"), RECIPE).unwrap();
    for name in ["a.toml", "b.toml"] {
        fs::write(dir.path().join("recipes").join(name), RECIPE).unwrap();
    }
    fs::write(dir.path().join("suite.toml"), SUITE).unwrap();
    dir
}

fn source(path: &Path, kind: DocumentKind, digest: &str) -> PreviewSource {
    PreviewSource {
        path: path.to_path_buf(),
        digest: digest.into(),
        kind,
    }
}

#[test]
fn recipe_bytes_keep_source_root_artifacts_normalization_and_raw_digest() {
    let dir = tree();
    let original = dir.path().join("recipe.toml");
    let bytes = fs::read(&original).unwrap();
    let loaded = recipe::load(&original).unwrap();
    let adjacent = dir.path().join("new.toml");
    let parsed = recipe::parse_bytes(&adjacent, &bytes).unwrap();
    assert!(!adjacent.exists());
    assert_eq!(parsed.path, adjacent);
    assert_eq!(parsed.project_root, loaded.project_root);
    assert_eq!(parsed.target_artifact, loaded.target_artifact);
    assert_eq!(parsed.recipe.observe, loaded.recipe.observe);
    assert_eq!(parsed.digest, format!("{:x}", Sha256::digest(&bytes)));
    assert_eq!(parsed.digest, loaded.digest);
    let changed = RECIPE.replace("name = \"clean-only\"", "name = \"edited\"");
    assert_ne!(
        recipe::parse_bytes(&adjacent, changed.as_bytes())
            .unwrap()
            .digest,
        loaded.digest
    );
    assert!(recipe::parse_bytes(&adjacent, b"not = [toml").is_err());
    fs::write(&original, b"not = [toml").unwrap();
    assert_eq!(
        format!(
            "{:#}",
            recipe::parse_bytes(&adjacent, b"not = [toml").unwrap_err()
        ),
        format!("{:#}", recipe::load(&original).unwrap_err())
    );
}

#[test]
fn suite_bytes_keep_variant_roots_matrix_normalization_and_raw_digest() {
    let dir = tree();
    let original = dir.path().join("suite.toml");
    let bytes = fs::read(&original).unwrap();
    let loaded = comparison::load(&original).unwrap();
    let adjacent = dir.path().join("new.toml");
    let parsed = comparison::parse_bytes(&adjacent, &bytes).unwrap();
    assert!(!adjacent.exists());
    assert_eq!(parsed.path, adjacent);
    assert_eq!(parsed.project_root, loaded.project_root);
    assert_eq!(parsed.manifest.scopes[0].path, "/home/theseus");
    assert_eq!(
        parsed.manifest.scopes[0].path,
        loaded.manifest.scopes[0].path
    );
    assert_eq!(parsed.manifest.profiles, loaded.manifest.profiles);
    assert_eq!(parsed.manifest.scenarios, loaded.manifest.scenarios);
    assert_eq!(
        parsed.variants[0].recipe.path,
        loaded.variants[0].recipe.path
    );
    assert_eq!(
        parsed.variants[1].recipe.path,
        loaded.variants[1].recipe.path
    );
    assert_eq!(parsed.digest, format!("{:x}", Sha256::digest(&bytes)));
    assert_eq!(parsed.digest, loaded.digest);
    let changed = SUITE.replace("name='methods'", "name='edited'");
    assert_ne!(
        comparison::parse_bytes(&adjacent, changed.as_bytes())
            .unwrap()
            .digest,
        loaded.digest
    );
    assert!(comparison::parse_bytes(&adjacent, b"not = [toml").is_err());
    let invalid_matrix = SUITE.replace("scenarios=['clean']", "scenarios=['upgrade']");
    assert!(comparison::parse_bytes(&adjacent, invalid_matrix.as_bytes()).is_err());
    fs::write(&original, invalid_matrix.as_bytes()).unwrap();
    assert_eq!(
        format!(
            "{:#}",
            comparison::parse_bytes(&adjacent, invalid_matrix.as_bytes()).unwrap_err()
        ),
        format!("{:#}", comparison::load(&original).unwrap_err())
    );
    assert!(comparison::parse_bytes(&adjacent, &vec![b' '; MAX_DRAFT_BYTES + 1]).is_err());
}

#[test]
fn recipe_headless_load_retains_its_existing_size_policy() {
    let dir = tree();
    let path = dir.path().join("recipe.toml");
    let mut raw = RECIPE.as_bytes().to_vec();
    raw.extend_from_slice(format!("\n#{}", "x".repeat(MAX_DRAFT_BYTES)).as_bytes());
    fs::write(&path, &raw).unwrap();
    let loaded = recipe::load(&path).unwrap();
    assert_eq!(loaded.digest, format!("{:x}", Sha256::digest(&raw)));
    assert!(DraftDocument::open(source(&path, DocumentKind::Recipe, &loaded.digest)).is_err());
}

#[test]
fn open_requires_unchanged_utf8_source_within_editor_limit() {
    let dir = tree();
    let path = dir.path().join("recipe.toml");
    let accepted = recipe::load(&path).unwrap();
    let source = source(&path, DocumentKind::Recipe, &accepted.digest);
    let draft = DraftDocument::open(source.clone()).unwrap();
    assert_eq!(draft.text.as_bytes(), fs::read(&path).unwrap());
    fs::write(&path, RECIPE.replace("clean", "changed")).unwrap();
    assert!(DraftDocument::open(source.clone()).is_err());
    fs::write(&path, vec![b' '; MAX_DRAFT_BYTES + 1]).unwrap();
    assert!(DraftDocument::open(source.clone()).is_err());
    fs::write(&path, [0xff]).unwrap();
    assert!(DraftDocument::open(source).is_err());
}

#[test]
fn recipe_save_copy_writes_exact_bytes_exclusively_and_preserves_original() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tree();
    let path = dir.path().join("recipe.toml");
    let before = fs::read(&path).unwrap();
    let accepted = recipe::load(&path).unwrap();
    let mut draft =
        DraftDocument::open(source(&path, DocumentKind::Recipe, &accepted.digest)).unwrap();
    draft.text.push_str("\n# π comment\n");
    let saved = draft.save_copy("new.edited.toml").unwrap();
    let copy = dir.path().join("new.edited.toml");
    assert_eq!(fs::read(&copy).unwrap(), draft.text.as_bytes());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        fs::metadata(&copy).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let ValidatedDocument::Recipe(saved) = saved else {
        panic!("wrong kind")
    };
    assert_eq!(saved.path, copy);
    assert_eq!(
        saved.digest,
        format!("{:x}", Sha256::digest(draft.text.as_bytes()))
    );
    assert_eq!(saved.project_root, accepted.project_root);
    assert_eq!(saved.target_artifact, accepted.target_artifact);
    assert!(draft.save_copy("new.edited.toml").is_err());
    assert!(draft.save_copy("recipe.toml").is_err());
    assert!(draft.save_copy("../elsewhere.toml").is_err());
    let longest_name = format!("{}.toml", "a".repeat(123));
    assert!(draft.save_copy(&longest_name).is_ok());
}

#[test]
fn suite_save_copy_reloads_variants_from_original_root() {
    let dir = tree();
    let path = dir.path().join("suite.toml");
    let accepted = comparison::load(&path).unwrap();
    let mut draft =
        DraftDocument::open(source(&path, DocumentKind::Suite, &accepted.digest)).unwrap();
    draft.text.push_str("\n# unchanged matrix\n");
    let ValidatedDocument::Suite(saved) = draft.save_copy("suite.edited.toml").unwrap() else {
        panic!("wrong kind")
    };
    assert_eq!(saved.path, dir.path().join("suite.edited.toml"));
    assert_eq!(
        saved.variants[0].recipe.path,
        accepted.variants[0].recipe.path
    );
    assert_eq!(
        saved.digest,
        format!("{:x}", Sha256::digest(draft.text.as_bytes()))
    );
}

#[test]
fn save_rejects_bad_names_collisions_and_invalid_drafts_without_creation() {
    use std::os::unix::fs::symlink;
    let dir = tree();
    let path = dir.path().join("recipe.toml");
    let accepted = recipe::load(&path).unwrap();
    let mut draft =
        DraftDocument::open(source(&path, DocumentKind::Recipe, &accepted.digest)).unwrap();
    let existing = dir.path().join("existing.toml");
    fs::write(&existing, "sentinel").unwrap();
    let link = dir.path().join("link.toml");
    symlink(&existing, &link).unwrap();
    for name in [
        "",
        ".",
        "..",
        "bad",
        "recipe.toml",
        "../escape.toml",
        "/absolute.toml",
        "é.toml",
        "bad name.toml",
        "bad\nname.toml",
        "existing.toml",
        "link.toml",
    ] {
        assert!(draft.save_copy(name).is_err(), "accepted {name:?}");
    }
    assert!(
        draft
            .save_copy(&format!("{}.toml", "a".repeat(124)))
            .is_err()
    );
    assert_eq!(fs::read_to_string(&existing).unwrap(), "sentinel");
    assert_eq!(fs::read_link(&link).unwrap(), existing);
    draft.text = "not = [toml".into();
    assert!(draft.validate().is_err());
    assert!(draft.save_copy("invalid.toml").is_err());
    assert!(!dir.path().join("invalid.toml").exists());
    draft.text = format!("{}\n#{}", RECIPE, "x".repeat(MAX_DRAFT_BYTES));
    assert!(draft.validate().is_err());
    assert!(draft.save_copy("oversize.toml").is_err());
    assert!(!dir.path().join("oversize.toml").exists());
}
