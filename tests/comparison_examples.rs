use std::{path::Path, process::Command};
#[test]
fn public_comparison_examples_validate_with_both_profiles_and_same_app() {
    for name in ["methods", "images"] {
        let suite =
            theseus::comparison::load(Path::new(&format!("examples/comparisons/{name}.toml")))
                .unwrap();
        assert_eq!(suite.manifest.profiles.len(), 2);
        assert_eq!(suite.variants.len(), 2);
        for variant in &suite.variants {
            let out = Command::new(env!("CARGO_BIN_EXE_theseus"))
                .args(["validate", "--recipe"])
                .arg(&variant.recipe.path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
