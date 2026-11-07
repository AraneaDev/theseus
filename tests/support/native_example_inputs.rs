//! Data-only pre-Prepare fixture inputs; never an execution or recovery authority.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use theseus_host::fs::{HostDir, LeafOpenResult, RootPolicy, fingerprint};

const JSON_LIMIT: u64 = 1024 * 1024;
const FILES: [&str; 5] = [
    "recipe.json",
    "config.json",
    "launch.json",
    "provider-template.json",
    "graph.json",
];

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read_original(root: &HostDir, name: &str) -> Result<Vec<u8>> {
    let open = || match root.open_leaf_classified(OsStr::new(name)) {
        LeafOpenResult::Opened(file) => Ok(file),
        other => anyhow::bail!("original example input {name} unavailable: {other:?}"),
    };
    let mut file = open()?;
    let before = fingerprint(&file)?;
    let mut bytes = Vec::new();
    (&mut file).take(JSON_LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(
        !bytes.is_empty() && bytes.len() as u64 <= JSON_LIMIT,
        "bounded original JSON input required: {name}"
    );
    ensure!(
        before == fingerprint(&file)? && before == fingerprint(&open()?)?,
        "original example input changed: {name}"
    );
    Ok(bytes)
}

/// Capture exact constructor inputs after its final graph and before Prepare.
/// Existing graph snapshots are never replaced; incomplete captures fail closed.
pub fn capture(root: &Path) -> Result<PathBuf> {
    ensure!(
        root.is_absolute(),
        "absolute original fixture root required"
    );
    // TempDir owns the private root; retain it without following its leaf, while
    // allowing the same platform ancestor aliases as the fixture constructors.
    let directory = HostDir::open(root, RootPolicy::LeafNoFollow)?;
    let graph_bytes = read_original(&directory, "graph.json")?;
    let graph: Value = serde_json::from_slice(&graph_bytes)?;
    ensure!(
        graph["kind"] == "theseus_native_integration_fixture_graph" && graph["schema"] == 1,
        "original fixture graph required"
    );
    let mut captured = Vec::new();
    for name in FILES {
        let bytes = if name == "graph.json" {
            graph_bytes.clone()
        } else {
            read_original(&directory, name)?
        };
        captured.push((name, bytes));
    }
    if graph.get("broker_config").is_some() {
        captured.push(("broker.json", read_original(&directory, "broker.json")?));
    }
    for (role, name) in [
        ("recipe", "recipe.json"),
        ("config", "config.json"),
        ("launch", "launch.json"),
        ("provider_template", "provider-template.json"),
        ("broker_config", "broker.json"),
    ] {
        if role == "broker_config" && graph.get(role).is_none() {
            continue;
        }
        let bytes = &captured
            .iter()
            .find(|(key, _)| *key == name)
            .context("original captured input")?
            .1;
        let pin = &graph[role];
        ensure!(
            pin["path"].as_str().map(Path::new) == Some(root.join(name).as_path())
                && pin["length"].as_u64() == Some(bytes.len() as u64)
                && pin["sha256"].as_str() == Some(digest(bytes).as_str()),
            "original graph pin mismatch: {role}"
        );
    }
    ensure!(
        read_original(&directory, "graph.json")? == graph_bytes,
        "original graph changed during capture"
    );
    ensure!(
        directory.same_directory_object(&HostDir::open(root, RootPolicy::LeafNoFollow)?)?,
        "original fixture root mapping changed"
    );
    // Validate every input before creating any snapshot directory. The held
    // child handles reject links; exclusively creating the graph directory
    // makes every publication below a first write before any runtime actor.
    let parent_name = OsStr::new("example-inputs");
    let parent = match directory.create_child(parent_name) {
        Ok(parent) => parent,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            directory.open_child_name(parent_name)?
        }
        Err(error) => return Err(error.into()),
    };
    let graph_sha = digest(&graph_bytes);
    let snapshot = parent.create_child(OsStr::new(&graph_sha))?;
    let mut rows = Vec::new();
    for (name, bytes) in captured {
        snapshot.publish(OsStr::new(name), &mut |file| file.write_all(&bytes))?;
        rows.push(json!({"name":name,"length":bytes.len(),"sha256":digest(&bytes)}));
    }
    let manifest = serde_json::to_vec_pretty(&json!({
        "kind":"theseus_native_example_inputs","schema":1,
        "original_fixture_root":root,"graph_sha256":graph_sha,"files":rows,
    }))?;
    // Written last: an interrupted capture cannot appear complete to export.
    snapshot.publish(OsStr::new("inputs.json"), &mut |file| {
        file.write_all(&manifest)
    })?;
    Ok(root.join("example-inputs").join(graph_sha))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    fn write_graph(root: &Path, broker: bool) {
        for name in [
            "recipe.json",
            "config.json",
            "launch.json",
            "provider-template.json",
        ] {
            std::fs::write(
                root.join(name),
                format!("{{\"original_input\":\"{name}\"}}"),
            )
            .unwrap();
        }
        let mut graph = json!({"kind":"theseus_native_integration_fixture_graph","schema":1});
        for (role, name) in [
            ("recipe", "recipe.json"),
            ("config", "config.json"),
            ("launch", "launch.json"),
            ("provider_template", "provider-template.json"),
        ] {
            let path = root.join(name);
            let bytes = std::fs::read(&path).unwrap();
            graph[role] = json!({"path":path,"length":bytes.len(),"sha256":digest(&bytes)});
        }
        if broker {
            let path = root.join("broker.json");
            std::fs::write(&path, b"{\"original_broker\":true}").unwrap();
            let bytes = std::fs::read(&path).unwrap();
            graph["broker_config"] =
                json!({"path":path,"length":bytes.len(),"sha256":digest(&bytes)});
        }
        std::fs::write(root.join("graph.json"), serde_json::to_vec(&graph).unwrap()).unwrap();
    }
    #[test]
    fn snapshot_preserves_original_graph_pins_after_provider_atomic_replacement() {
        let temp = tempfile::tempdir().unwrap();
        write_graph(temp.path(), true);
        let graph = std::fs::read(temp.path().join("graph.json")).unwrap();
        let original = std::fs::read(temp.path().join("config.json")).unwrap();
        let snapshot = capture(temp.path()).unwrap();
        assert_eq!(snapshot.file_name().unwrap(), digest(&graph).as_str());
        let prepared = temp.path().join("prepared.json");
        std::fs::write(&prepared, b"{\"provider_prepared\":true}").unwrap();
        std::fs::rename(&prepared, temp.path().join("config.json")).unwrap();
        assert_eq!(
            std::fs::read(snapshot.join("config.json")).unwrap(),
            original
        );
        assert_eq!(std::fs::read(snapshot.join("graph.json")).unwrap(), graph);
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(snapshot.join("inputs.json")).unwrap()).unwrap();
        assert_eq!(
            manifest["original_fixture_root"],
            temp.path().display().to_string()
        );
        assert_eq!(manifest["graph_sha256"], digest(&graph));
        assert_eq!(manifest["files"].as_array().unwrap().len(), 6);
        for row in manifest["files"].as_array().unwrap() {
            let bytes = std::fs::read(snapshot.join(row["name"].as_str().unwrap())).unwrap();
            assert_eq!(row["sha256"], digest(&bytes));
            assert_eq!(row["length"], bytes.len());
        }
    }
    #[test]
    fn distinct_final_graphs_keep_independent_inputs_and_never_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        write_graph(temp.path(), false);
        let first = capture(temp.path()).unwrap();
        let first_bytes = std::fs::read(first.join("graph.json")).unwrap();
        assert!(
            capture(temp.path()).is_err(),
            "existing graph snapshot must not be overwritten"
        );
        write_graph(temp.path(), true);
        let second = capture(temp.path()).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            std::fs::read(first.join("graph.json")).unwrap(),
            first_bytes
        );
        assert!(second.join("broker.json").exists());
    }
    #[test]
    fn recipe_bytes_and_original_path_require_the_independent_graph_pin() {
        for damage in ["bytes", "missing", "null", "path"] {
            let temp = tempfile::tempdir().unwrap();
            write_graph(temp.path(), false);
            if damage == "bytes" {
                std::fs::write(
                    temp.path().join("recipe.json"),
                    b"changed recipe before capture",
                )
                .unwrap();
            } else {
                let path = temp.path().join("graph.json");
                let mut graph: Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                match damage {
                    "missing" => {
                        graph.as_object_mut().unwrap().remove("recipe");
                    }
                    "null" => graph["recipe"] = Value::Null,
                    _ => graph["recipe"]["path"] = json!(temp.path().join("other-recipe.json")),
                }
                std::fs::write(path, serde_json::to_vec(&graph).unwrap()).unwrap();
            }
            assert!(capture(temp.path()).is_err(), "{damage}");
            assert!(!temp.path().join("example-inputs").exists(), "{damage}");
        }
    }
    #[test]
    fn changed_original_pin_refuses_before_snapshot_creation() {
        let temp = tempfile::tempdir().unwrap();
        write_graph(temp.path(), false);
        std::fs::write(temp.path().join("config.json"), b"changed before capture").unwrap();
        assert!(capture(temp.path()).is_err());
        assert!(!temp.path().join("example-inputs").exists());
    }
}
