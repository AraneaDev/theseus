#!/usr/bin/env python3
"""Pin production, native and supervised MCP executables before test rebuilds.

Print only the trusted native fixture path on stdout. Cargo diagnostics go to
stderr. artifacts.json identifies each distinct graph and copied executable.
"""
import hashlib
import importlib.util
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
ROOT = pathlib.Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('theseus_validation', ROOT / 'scripts/validate.py')
VALIDATION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VALIDATION)


def build(arguments):
    result = subprocess.run(['cargo', 'build', '--locked', *arguments, '--message-format=json'],
                            cwd=ROOT, stdout=subprocess.PIPE, text=True, check=False)
    artifacts = []
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if message.get('reason') == 'compiler-message':
            print(message['message'].get('rendered', ''), end='', file=sys.stderr)
        if message.get('reason') == 'compiler-artifact':
            artifacts.append(message)
    result.check_returncode()
    return artifacts


def pin(owned, artifacts, target, key):
    matches = [artifact for artifact in artifacts
               if artifact['target']['name'] == target and artifact.get('executable')]
    if len(matches) != 1:
        raise RuntimeError(f'Cargo did not identify exactly one executable for {target}')
    artifact = matches[0]
    source = pathlib.Path(artifact['executable'])
    destination = owned / key
    destination.mkdir()
    copied = destination / source.name
    digest = hashlib.sha256(source.read_bytes()).hexdigest()
    shutil.copy2(source, copied)
    if hashlib.sha256(copied.read_bytes()).hexdigest() != digest:
        raise RuntimeError(f'Native artifact copy changed bytes: {key}')
    return {'path': str(copied), 'sha256': digest, 'cargo_artifact': artifact}


def main():
    identity = VALIDATION.source_identity(ROOT)
    owned = pathlib.Path(tempfile.mkdtemp(prefix='theseus-native-artifacts-')).resolve()
    metadata = dict(identity)
    # Pin before any feature build can overwrite target/debug/theseus.
    production = build(['--bin', 'theseus'])
    hosts = [artifact for artifact in production if artifact['target']['name'] == 'theseus_host']
    apps = [artifact for artifact in production if artifact['target']['name'] == 'theseus']
    if not hosts or not apps or any('test-support' in artifact['features'] for artifact in hosts) or any('mcp-test-support' in artifact['features'] for artifact in apps):
        raise RuntimeError('Production graph contains fault support or required artifact is absent')
    metadata['production'] = pin(owned, production, 'theseus', 'production')
    metadata['production_graph'] = production
    helper = build(['-p', 'theseus-native-helper', '--bin', 'theseus-native-helper'])
    VALIDATION.check_helper_graph(helper)
    metadata['helper_production'] = pin(owned, helper, 'theseus-native-helper', 'helper-production')
    metadata['helper_production_graph'] = helper
    native = build(['--workspace', '--bins', '--features', 'theseus-host/test-support'])
    metadata['fixture'] = pin(owned, native, 'theseus-host-fixture', 'fixture')
    metadata['application'] = pin(owned, native, 'theseus', 'application')
    mcp = build(['--features', 'mcp-test-support', '--bin', 'theseus-mcp-fixture'])
    metadata['mcp_fixture'] = pin(owned, mcp, 'theseus-mcp-fixture', 'mcp-fixture')
    helper_fixture = build(['-p', 'theseus-native-helper', '--features', 'test-support', '--bin', 'theseus-native-helper-fixture'])
    VALIDATION.check_helper_graph(helper_fixture, fixture=True)
    metadata['helper_fixture'] = pin(owned, helper_fixture, 'theseus-native-helper-fixture', 'helper-fixture')
    metadata['helper_fixture_graph'] = helper_fixture
    if VALIDATION.source_identity(ROOT) != identity:
        raise RuntimeError('Source changed while building pinned artifacts; no source binding established')
    # Preserve the original fixture-parent metadata convention for Rust tests.
    for directory in (owned, pathlib.Path(metadata['fixture']['path']).parent):
        (directory / 'artifacts.json').write_text(json.dumps(metadata, indent=2) + '\n', encoding='utf-8')
    print(metadata['fixture']['path'])


if __name__ == '__main__':
    main()
