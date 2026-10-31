#!/usr/bin/env python3
"""Build the trusted native fixture and print Cargo's executable artifact path.

Set THESEUS_HOST_TEST_FIXTURE to this output before running application tests.
Cargo diagnostics stay on stderr, making stdout suitable for environment capture.
"""
import hashlib
import shutil
import tempfile
import json
import pathlib
import subprocess
import sys


def main():
    root = pathlib.Path(__file__).resolve().parents[2]
    result = subprocess.run(
        ["cargo", "build", "--locked", "--workspace", "--bins",
         "--features", "theseus-host/test-support", "--message-format=json"],
        cwd=root, stdout=subprocess.PIPE, text=True, check=False,
    )
    artifacts = {}
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-message":
            print(message["message"].get("rendered", ""), end="", file=sys.stderr)
        if (message.get("reason") == "compiler-artifact"
                and message["target"]["name"] in ("theseus-host-fixture", "theseus")
                and message.get("executable")):
            artifacts[message["target"]["name"]] = message
    result.check_returncode()
    if set(artifacts) != {'theseus', 'theseus-host-fixture'}:
        raise RuntimeError("Cargo did not identify both native executables")
    owned = pathlib.Path(tempfile.mkdtemp(prefix='theseus-native-artifacts-')).resolve()
    metadata = {'source_sha': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()}
    for name, key in [('theseus-host-fixture', 'fixture'), ('theseus', 'application')]:
        artifact = artifacts[name]
        source = pathlib.Path(artifact['executable'])
        copied = owned / source.name
        shutil.copy2(source, copied)
        digest = hashlib.sha256(source.read_bytes()).hexdigest()
        if hashlib.sha256(copied.read_bytes()).hexdigest() != digest:
            raise RuntimeError('native artifact copy changed bytes')
        metadata[key] = {'path': str(copied), 'sha256': digest, 'cargo_artifact': artifact}
    (owned / 'artifacts.json').write_text(json.dumps(metadata, indent=2), encoding='utf-8')
    print(metadata['fixture']['path'])


if __name__ == "__main__":
    main()
