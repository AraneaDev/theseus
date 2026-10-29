//! Shared acceptance boundary: only the daemon's exact missing-target response
//! proves cleanup. CLI/transport failures never establish container absence.
use std::process::{Command, Output};

pub fn container_is_absent(id: &str, output: &Output) -> bool {
    let error = String::from_utf8_lossy(&output.stderr)
        .trim()
        .to_ascii_lowercase();
    output.status.code() == Some(1)
        && [
            format!("error: no such object: {id}"),
            format!("error response from daemon: no such container: {id}"),
        ]
        .contains(&error)
}

pub fn absent(id: &str) {
    let output = Command::new("docker")
        .args(["inspect", id])
        .output()
        .unwrap();
    assert!(
        container_is_absent(id, &output),
        "Docker inspect did not prove {id} absent: status={:?}, stdout={:?}, stderr={:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout)
            .chars()
            .take(1024)
            .collect::<String>(),
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(1024)
            .collect::<String>(),
    );
}
