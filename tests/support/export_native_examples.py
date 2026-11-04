#!/usr/bin/env python3
"""Export complete fixture examples without launching a helper, provider or app."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat

FILES = ('recipe.json', 'config.json', 'launch.json', 'provider-template.json', 'graph.json')
LIMIT = 512 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def measured(path, *, capture=False):
    path = Path(path)
    before = path.lstat()
    require(stat.S_ISREG(before.st_mode) and 0 < before.st_size <= LIMIT,
            'bounded regular original file required: ' + str(path))
    require(not capture or before.st_size <= 1024 * 1024, 'bounded JSON example required')
    digest = hashlib.sha256()
    raw = bytearray()
    with path.open('rb') as stream:
        opened = os.fstat(stream.fileno())
        require((opened.st_dev, opened.st_ino) == (before.st_dev, before.st_ino), 'original mapping changed')
        while True:
            chunk = stream.read(65536)
            if not chunk:
                break
            digest.update(chunk)
            if capture:
                raw.extend(chunk)
        after = os.fstat(stream.fileno())
    signature = lambda value: (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns)
    require(signature(before) == signature(after) == signature(path.lstat()), 'original file changed')
    return {'length': before.st_size, 'sha256': digest.hexdigest()}, bytes(raw)


def check_pin(pin, actual):
    require(isinstance(pin, dict) and type(pin.get('length')) is int
            and pin['length'] == actual['length'] and pin.get('sha256') == actual['sha256'],
            'artifact/example pin does not match original bytes')


def export(root, metadata, output):
    root = Path(root).resolve(strict=True)
    output = Path(output)
    require(re.fullmatch(r'[a-f0-9]{40}|[a-f0-9]{64}', metadata.get('source_sha', '')),
            'original fixture-builder source SHA required')
    require(re.fullmatch(r'[a-f0-9]{64}', metadata.get('source_tree_sha256', '')),
            'original fixture-builder source tree pin required')
    captured = {name: measured(root / name, capture=True) for name in FILES}
    graph = json.loads(captured['graph.json'][1])
    recipe = json.loads(captured['recipe.json'][1])
    require(graph.get('kind') == 'theseus_native_integration_fixture_graph' and graph.get('schema') == 1,
            'compiled integration fixture graph required')
    require(recipe.get('kind') == 'theseus_native_recipe' and recipe.get('schema') == 2,
            'complete schema2 integration recipe required')
    catalog = recipe.get('catalog', {})
    require(catalog.get('os') in ('linux', 'macos', 'windows'), 'explicit target OS required')
    for key, expected in [('integration_manifest_digest', recipe.get('integration_manifest_digest')),
                          ('manifest_digest', catalog.get('manifest_digest'))]:
        require(isinstance(expected, str) and re.fullmatch(r'[a-f0-9]{64}', expected)
                and expected != '0' * 64 and graph.get(key) == expected,
                'matching real manifest commitments required')
    roles = {'base': 'native_service_base', 'target': 'native_service_target',
             'controller': 'native_integration_controller', 'provider': 'native_service_fixture',
             'helper': 'helper_fixture' if graph.get('source_mode') else 'helper_production'}
    if graph.get('source_mode'):
        require(graph['source_mode'] in ('source_owned_desktop', 'source_owned_combined'), 'unknown source mode')
        roles['broker'] = 'desktop_broker_fixture'
    artifacts = {}
    for role, key in roles.items():
        pin = graph.get(role)
        require(isinstance(pin, dict) and isinstance(pin.get('path'), str), 'missing original role: ' + role)
        actual, _ = measured(pin['path'])
        check_pin(pin, actual)
        expected = metadata.get(key)
        require(isinstance(expected, dict) and expected.get('sha256') == actual['sha256'],
                'compiled fixture builder artifact mismatch: ' + role)
        if 'length' in expected:
            check_pin(expected, actual)
        artifacts[role] = actual
    for role in ('base', 'target'):
        check_pin(catalog.get(role), artifacts[role])
    configs = {'config': 'config.json', 'launch': 'launch.json', 'provider_template': 'provider-template.json'}
    if 'broker_config' in graph:
        captured['broker.json'] = measured(root / 'broker.json', capture=True)
        configs['broker_config'] = 'broker.json'
    for role, name in configs.items():
        pin = graph.get(role)
        require(isinstance(pin, dict) and Path(pin.get('path', '')).resolve() == (root / name),
                'example config must be the graph original: ' + role)
        check_pin(pin, captured[name][0])
    # Complete all reads/checks before creating output. Preserve literal paths,
    # digests and bytes: relocation cannot silently rewrite a committed recipe.
    output.mkdir(mode=0o700, parents=False, exist_ok=False)
    files = []
    for name, (pin, raw) in captured.items():
        with (output / name).open('xb') as stream:
            stream.write(raw)
        files.append({'name': name, **pin})
    manifest = {'kind': 'theseus_native_fixture_examples', 'schema': 1, 'os': catalog['os'],
                'source_sha': metadata['source_sha'], 'source_tree_sha256': metadata['source_tree_sha256'],
                'original_fixture_root': str(root), 'files': files, 'compiled_artifacts': artifacts}
    with (output / 'examples.json').open('x', encoding='utf-8') as stream:
        json.dump(manifest, stream, indent=2)
        stream.write('\n')
    verify(output)
    return manifest


def verify(output):
    output = Path(output)
    _, raw = measured(output / 'examples.json', capture=True)
    manifest = json.loads(raw)
    require(manifest.get('kind') == 'theseus_native_fixture_examples' and manifest.get('schema') == 1,
            'example manifest required')
    rows = manifest.get('files', [])
    names = [row.get('name') for row in rows]
    require(len(names) == len(set(names)) and set(FILES).issubset(names)
            and set(names).issubset({*FILES, 'broker.json'}), 'exact example file set required')
    for row in rows:
        actual, _ = measured(output / row['name'])
        check_pin(row, actual)
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--verify', type=Path, help='verify exported bytes without original target/artifact access')
    parser.add_argument('--fixture-root', type=Path)
    parser.add_argument('--artifacts', type=Path, help='fixture-builder artifacts.json or native-artifacts.json')
    parser.add_argument('--output', type=Path, help='new output directory; original absolute paths stay unchanged')
    args = parser.parse_args()
    if args.verify:
        require(not (args.fixture_root or args.artifacts or args.output), '--verify is exclusive')
        result = verify(args.verify)
    else:
        require(args.fixture_root and args.artifacts and args.output, 'fixture root, artifact metadata and new output required')
        result = export(args.fixture_root, json.loads(args.artifacts.read_text(encoding='utf-8')), args.output)
    print(json.dumps({'os': result['os'], 'source_sha': result['source_sha'], 'files': len(result['files'])}))


if __name__ == '__main__':
    main()
