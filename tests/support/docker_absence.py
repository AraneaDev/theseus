"""Require explicit missing-target evidence from an independent Docker inspect."""


def require_container_absent(container_id, result):
    expected = {
        f"error: no such object: {container_id}",
        f"error response from daemon: no such container: {container_id}",
    }
    assert result.returncode == 1 and result.stderr.strip().lower() in expected, (
        f"Docker inspect did not prove {container_id} absent: "
        f"status={result.returncode}, stdout={result.stdout[:1024]!r}, "
        f"stderr={result.stderr[:1024]!r}"
    )
