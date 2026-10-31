#!/usr/bin/env python3
"""Build the trusted native fixture and print Cargo's executable artifact path.

Set THESEUS_HOST_TEST_FIXTURE to this output before running application tests.
Cargo diagnostics stay on stderr, making stdout suitable for environment capture.
"""
import json
import pathlib
import subprocess
import sys


def main():
    root = pathlib.Path(__file__).resolve().parents[2]
    result = subprocess.run(
        ["cargo", "build", "--locked", "-p", "theseus-host", "--bin",
         "theseus-host-fixture", "--message-format=json"],
        cwd=root, stdout=subprocess.PIPE, text=True, check=False,
    )
    paths = []
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-message":
            print(message["message"].get("rendered", ""), end="", file=sys.stderr)
        if (message.get("reason") == "compiler-artifact"
                and message["target"]["name"] == "theseus-host-fixture"
                and message.get("executable")):
            paths.append(message["executable"])
    result.check_returncode()
    if len(paths) != 1 or not pathlib.Path(paths[0]).is_file():
        raise RuntimeError("Cargo did not identify exactly one native fixture executable")
    print(paths[0])


if __name__ == "__main__":
    main()
