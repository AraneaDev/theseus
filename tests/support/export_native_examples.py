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



def commitment(value, label):
    require(isinstance(value, str) and re.fullmatch(r'[a-f0-9]{64}', value)
            and value != '0' * 64, 'real nonzero ' + label + ' commitment required')
    return value


def integration_manifest_digest(core, integration):
    # Protocol primary source: crates/theseus-native-protocol/src/v2/types.rs,
    # integration_manifest_digest: SHA256(domain-NUL || raw core || raw integration).
    core = commitment(core, 'core')
    integration = commitment(integration, 'integration')
    return hashlib.sha256(b'theseus.native_integration_v1.manifest\0'
                          + bytes.fromhex(core) + bytes.fromhex(integration)).hexdigest()


def check_context_bindings(graph, captured, core, integration, full):
    config = json.loads(captured['config.json'][1])
    launch = json.loads(captured['launch.json'][1])
    provider = json.loads(captured['provider-template.json'][1])
    require(all(isinstance(value, dict) for value in (config, launch, provider)),
            'original config/launch/provider binding objects required')
    expected = {'core_manifest_digest': core, 'integration_manifest_digest': integration,
                'manifest_digest': full}
    for key, value in expected.items():
        require(config.get(key) == value, 'config ' + key + ' commitment mismatch')
        if key in launch:
            require(launch[key] == value, 'launch ' + key + ' commitment mismatch')
    require(launch.get('manifest_digest') == full, 'launch full commitment mismatch')
    require(provider.get('manifest_digest') == full, 'provider full commitment mismatch')
    manifest = config.get('integration_manifest')
    context = config.get('runtime_context')
    require(isinstance(manifest, dict) and manifest.get('core_manifest_digest') == core,
            'config manifest core commitment mismatch')
    require(isinstance(context, dict) and context.get('manifest_digest') == full
            and context.get('integration_manifest_digest') == integration,
            'runtime context component/full commitment mismatch')
    for key, size in (('binding', 32), ('workspace_marker', 32), ('provider_epoch', 64)):
        value = graph.get(key)
        require(isinstance(value, str) and re.fullmatch(r'[a-f0-9]{' + str(size) + '}', value)
                and value != '0' * size, 'real graph ' + key + ' binding required')
        require(config.get(key) == context.get(key) == value, 'original config/context ' + key + ' binding mismatch')
        if key in launch:
            require(launch[key] == value, 'launch ' + key + ' binding mismatch')
        if key != 'provider_epoch':
            require(provider.get(key) == value, 'provider ' + key + ' binding mismatch')
    require(provider.get('reset_epoch') == graph['provider_epoch'], 'provider original epoch mismatch')
    if 'broker.json' in captured:
        broker = json.loads(captured['broker.json'][1])
        require(isinstance(broker, dict) and broker.get('manifest') == manifest,
                'broker original manifest commitment mismatch')
        header = broker.get('header')
        require(isinstance(header, dict) and header == context.get('desktop_broker_header'),
                'broker original context/header binding mismatch')
        require(header.get('manifest_digest') == full, 'broker full commitment mismatch')



def captured_inputs(root):
    original_graph = measured(root / 'graph.json', capture=True)
    graph = json.loads(original_graph[1])
    names = (*FILES, 'broker.json') if 'broker_config' in graph else FILES
    parent = root / 'example-inputs'
    snapshot = parent / original_graph[0]['sha256']
    if not os.path.lexists(snapshot):
        return {name: original_graph if name == 'graph.json' else measured(root / name, capture=True)
                for name in names}, None
    require(parent.resolve() == parent and snapshot.resolve() == snapshot
            and stat.S_ISDIR(snapshot.lstat().st_mode), 'original input snapshot directory required')
    manifest_pin, raw = measured(snapshot / 'inputs.json', capture=True)
    manifest = json.loads(raw)
    require(isinstance(manifest, dict) and manifest.get('kind') == 'theseus_native_example_inputs'
            and manifest.get('schema') == 1
            and Path(manifest.get('original_fixture_root', '')).resolve(strict=True) == root
            and manifest.get('graph_sha256') == original_graph[0]['sha256'],
            'original input snapshot graph/root binding mismatch')
    rows = manifest.get('files')
    require(isinstance(rows, list) and all(isinstance(row, dict)
            and set(row) == {'name', 'length', 'sha256'} for row in rows),
            'exact original input snapshot rows required')
    require(len(rows) == len(names) and {row['name'] for row in rows} == set(names),
            'exact original input snapshot file set required')
    captured = {}
    for row in rows:
        actual, data = measured(snapshot / row['name'], capture=True)
        check_pin(row, actual)
        captured[row['name']] = (actual, data)
    require(captured['graph.json'] == original_graph
            and measured(root / 'graph.json', capture=True) == original_graph,
            'original snapshot/current graph changed')
    return captured, {'path': str(snapshot), 'graph_sha256': original_graph[0]['sha256'],
                      'inputs_manifest_sha256': manifest_pin['sha256']}


def export(root, metadata, output):
    root = Path(root).resolve(strict=True)
    output = Path(output)
    require(re.fullmatch(r'[a-f0-9]{40}|[a-f0-9]{64}', metadata.get('source_sha', '')),
            'original fixture-builder source SHA required')
    require(re.fullmatch(r'[a-f0-9]{64}', metadata.get('source_tree_sha256', '')),
            'original fixture-builder source tree pin required')
    captured, snapshot = captured_inputs(root)
    graph = json.loads(captured['graph.json'][1])
    recipe = json.loads(captured['recipe.json'][1])
    require(graph.get('kind') == 'theseus_native_integration_fixture_graph' and graph.get('schema') == 1,
            'compiled integration fixture graph required')
    require(recipe.get('kind') == 'theseus_native_recipe' and recipe.get('schema') == 2,
            'complete schema2 integration recipe required')
    catalog = recipe.get('catalog', {})
    require(catalog.get('os') in ('linux', 'macos', 'windows'), 'explicit target OS required')
    core = commitment(catalog.get('manifest_digest'), 'catalog core')
    integration = commitment(recipe.get('integration_manifest_digest'), 'recipe integration')
    full = integration_manifest_digest(core, integration)
    for key, expected in (('core_manifest_digest', core),
                          ('integration_manifest_digest', integration), ('manifest_digest', full)):
        require(graph.get(key) == expected, 'matching graph ' + key + ' commitment required')
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
    configs = {'recipe': 'recipe.json', 'config': 'config.json', 'launch': 'launch.json', 'provider_template': 'provider-template.json'}
    if 'broker_config' in graph:
        configs['broker_config'] = 'broker.json'
    for role, name in configs.items():
        pin = graph.get(role)
        if role == 'recipe':
            path = pin.get('path') if isinstance(pin, dict) else None
            require(isinstance(path, str) and Path(path).is_absolute()
                    and Path(path).name == name and Path(path).parent.resolve() == root,
                    'recipe pin must name original root/recipe.json')
        require(isinstance(pin, dict) and Path(pin.get('path', '')).resolve() == (root / name),
                'example config must be the graph original: ' + role)
        check_pin(pin, captured[name][0])
    check_context_bindings(graph, captured, core, integration, full)
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
    if snapshot is not None:
        manifest['input_snapshot'] = snapshot
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
