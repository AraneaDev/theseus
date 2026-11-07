"""Read-only example packaging controls; no target or application is started."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('examples', Path(__file__).with_name('export_native_examples.py'))
examples = importlib.util.module_from_spec(spec)
spec.loader.exec_module(examples)


# Real protocol-produced component/full vector from the retained passing 7e5b Upgrade case.
CORE = 'fe7c6f76e994f25e57fb17f892076f478a9ad050088426c5d7bc9e64125b1177'
INTEGRATION = 'da06ff26d96d9cd8bdc0cdfe00b5d640ba6a1f66a8ae4af1d73f20a5c780c9b1'
FULL = '0a49b129400381b0fc6d827c8281b55146553596c6f9553bda3529b3b1e5f4b5'

def sha(raw):
    return hashlib.sha256(raw).hexdigest()


class ExportExamples(unittest.TestCase):
    def fixture(self, root, os_name='linux'):
        root.mkdir()
        keys = {'base': 'native_service_base', 'target': 'native_service_target',
                'controller': 'native_integration_controller', 'helper': 'helper_production',
                'provider': 'native_service_fixture'}
        graph = {'kind': 'theseus_native_integration_fixture_graph', 'schema': 1,
                 'core_manifest_digest': CORE, 'manifest_digest': FULL, 'integration_manifest_digest': INTEGRATION,
                 'binding': '01' * 16, 'workspace_marker': '02' * 16, 'provider_epoch': '03' * 32}
        metadata = {'source_sha': 'c' * 40, 'source_tree_sha256': 'd' * 64}
        for role, key in keys.items():
            path = root / (role + '.bin')
            path.write_bytes(role.encode())
            pin = {'path': str(path), 'sha256': sha(path.read_bytes()), 'length': path.stat().st_size}
            graph[role] = pin
            metadata[key] = pin.copy()
        recipe = {'kind': 'theseus_native_recipe', 'schema': 2, 'integration_manifest_digest': INTEGRATION,
                  'catalog': {'os': os_name, 'manifest_digest': CORE,
                              'base': {k: graph['base'][k] for k in ('length', 'sha256')},
                              'target': {k: graph['target'][k] for k in ('length', 'sha256')}}}
        context = {k: graph[k] for k in ('binding', 'workspace_marker', 'provider_epoch', 'manifest_digest', 'integration_manifest_digest')}
        config = {k: graph[k] for k in ('binding', 'workspace_marker', 'provider_epoch', 'manifest_digest', 'core_manifest_digest', 'integration_manifest_digest')}
        config.update(schema=3, integration_manifest={'core_manifest_digest': CORE}, runtime_context=context)
        files = {'recipe.json': recipe, 'config.json': config,
                 'launch.json': {'fixture': 'explicit', 'manifest_digest': FULL},
                 'provider-template.json': {'schema': 1, 'binding': graph['binding'],
                                            'manifest_digest': FULL, 'workspace_marker': graph['workspace_marker'],
                                            'reset_epoch': graph['provider_epoch']}}
        for name, value in files.items():
            path = root / name
            path.write_text(json.dumps(value))
            role = {'recipe.json': 'recipe', 'config.json': 'config', 'launch.json': 'launch', 'provider-template.json': 'provider_template'}[name]
            graph[role] = {'path': str(path), 'sha256': sha(path.read_bytes()), 'length': path.stat().st_size}
        (root / 'graph.json').write_text(json.dumps(graph))
        return metadata

    def test_exports_complete_unchanged_configs_and_verifies_all_three_os(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            for os_name in ('linux', 'macos', 'windows'):
                root = base / os_name
                metadata = self.fixture(root, os_name)
                output = base / (os_name + '-examples')
                result = examples.export(root, metadata, output)
                self.assertEqual(result['os'], os_name)
                self.assertEqual(result['source_sha'], metadata['source_sha'])
                for name in ('graph.json', 'recipe.json', 'config.json', 'launch.json', 'provider-template.json'):
                    self.assertEqual((output / name).read_bytes(), (root / name).read_bytes())
                examples.verify(output)
                (output / 'recipe.json').write_text('{}')
                with self.assertRaises(ValueError):
                    examples.verify(output)

    def snapshot(self, root):
        graph = (root / 'graph.json').read_bytes()
        graph_digest = sha(graph)
        directory = root / 'example-inputs' / graph_digest
        directory.mkdir(mode=0o700, parents=True, exist_ok=False)
        files = []
        for name in (*examples.FILES, 'broker.json') if 'broker_config' in json.loads(graph) else examples.FILES:
            raw = (root / name).read_bytes()
            (directory / name).write_bytes(raw)
            files.append({'name': name, 'length': len(raw), 'sha256': sha(raw)})
        manifest = {'kind': 'theseus_native_example_inputs', 'schema': 1,
                    'original_fixture_root': str(root), 'graph_sha256': graph_digest, 'files': files}
        (directory / 'inputs.json').write_text(json.dumps(manifest))
        return directory

    def test_recipe_mutation_cannot_be_authorized_by_repinning_snapshot_manifest(self):
        mutations = [('steps', {'configure': {'program': 'exercise'}}),
                     ('checks', [{'command': {'stdout_literal': 'forged'}}]),
                     ('limits', {'run_seconds': 999})]
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            for index, (field, value) in enumerate(mutations):
                with self.subTest(field=field):
                    root = base / ('fixture-' + str(index))
                    metadata = self.fixture(root)
                    original_recipe = (root / 'recipe.json').read_bytes()
                    snapshot = self.snapshot(root)
                    recipe = json.loads(original_recipe)
                    recipe['catalog'][field] = value
                    tampered = json.dumps(recipe).encode()
                    (snapshot / 'recipe.json').write_bytes(tampered)
                    manifest = json.loads((snapshot / 'inputs.json').read_text())
                    for row in manifest['files']:
                        if row['name'] == 'recipe.json':
                            row.update(length=len(tampered), sha256=sha(tampered))
                    (snapshot / 'inputs.json').write_text(json.dumps(manifest))
                    output = base / ('bad-' + str(index))
                    with self.assertRaises(ValueError):
                        examples.export(root, metadata, output)
                    self.assertFalse(output.exists())
                    self.assertEqual((root / 'recipe.json').read_bytes(), original_recipe)

    def test_recipe_requires_its_original_graph_pin_and_root_path(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            for index, damage in enumerate(('missing', 'null', 'path')):
                with self.subTest(damage=damage):
                    root = base / ('fixture-' + str(index))
                    metadata = self.fixture(root)
                    graph = json.loads((root / 'graph.json').read_text())
                    if damage == 'missing':
                        del graph['recipe']
                    elif damage == 'null':
                        graph['recipe'] = None
                    else:
                        graph['recipe']['path'] = str(root / 'other-recipe.json')
                    (root / 'graph.json').write_text(json.dumps(graph))
                    output = base / ('bad-' + str(index))
                    with self.assertRaises(ValueError):
                        examples.export(root, metadata, output)
                    self.assertFalse(output.exists())

    def test_recipe_graph_pin_cannot_use_an_alias_leaf(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / 'fixture'
            metadata = self.fixture(root)
            alias = root / 'alias.json'
            try:
                alias.symlink_to(root / 'recipe.json')
            except (OSError, NotImplementedError):
                self.skipTest('symlink creation unavailable')
            graph = json.loads((root / 'graph.json').read_text())
            graph['recipe']['path'] = str(alias)
            (root / 'graph.json').write_text(json.dumps(graph))
            output = Path(temp) / 'bad'
            with self.assertRaises(ValueError):
                examples.export(root, metadata, output)
            self.assertFalse(output.exists())

    def test_exports_original_snapshot_after_provider_atomic_config_replacement(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / 'fixture'
            metadata = self.fixture(root)
            snapshot = self.snapshot(root)
            original = (snapshot / 'config.json').read_bytes()
            prepared = root / 'prepared-config.json'
            prepared.write_text('{"provider_prepared":true}')
            os.replace(prepared, root / 'config.json')
            output = Path(temp) / 'examples'
            result = examples.export(root, metadata, output)
            self.assertEqual((output / 'config.json').read_bytes(), original)
            self.assertEqual((output / 'graph.json').read_bytes(), (root / 'graph.json').read_bytes())
            # Export records the canonical root (Windows expands 8.3 temp names).
            self.assertEqual(result['original_fixture_root'], str(root.resolve(strict=True)))
            self.assertEqual(result['input_snapshot']['graph_sha256'], sha((root / 'graph.json').read_bytes()))
            self.assertEqual((root / 'config.json').read_text(), '{"provider_prepared":true}')
            examples.verify(output)

    def test_exports_broker_snapshot_and_keeps_original_artifact_checks(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / 'fixture'
            metadata = self.fixture(root)
            graph = json.loads((root / 'graph.json').read_text())
            config = json.loads((root / 'config.json').read_text())
            config['runtime_context']['desktop_broker_header'] = {'manifest_digest': FULL, 'binding': graph['binding']}
            broker = {'manifest': config['integration_manifest'], 'header': config['runtime_context']['desktop_broker_header']}
            for name, role, value in [('config.json', 'config', config), ('broker.json', 'broker_config', broker)]:
                raw = json.dumps(value).encode()
                (root / name).write_bytes(raw)
                graph[role] = {'path': str(root / name), 'length': len(raw), 'sha256': sha(raw)}
            (root / 'graph.json').write_text(json.dumps(graph))
            snapshot = self.snapshot(root)
            for name in ('config.json', 'broker.json'):
                prepared = root / 'prepared.json'
                prepared.write_text('{"prepared":true}')
                os.replace(prepared, root / name)
            output = Path(temp) / 'examples'
            examples.export(root, metadata, output)
            self.assertEqual((output / 'broker.json').read_bytes(), (snapshot / 'broker.json').read_bytes())
            (root / 'target.bin').write_bytes(b'substituted')
            with self.assertRaises(ValueError):
                examples.export(root, metadata, Path(temp) / 'bad')
            self.assertFalse((Path(temp) / 'bad').exists())

    def test_refuses_mutated_direct_inputs_and_repinned_snapshot_without_graph_authority(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / 'fixture'
            metadata = self.fixture(root)
            original = (root / 'config.json').read_bytes()
            (root / 'config.json').write_text('{"prepared":true}')
            with self.assertRaises(ValueError):
                examples.export(root, metadata, Path(temp) / 'direct-bad')
            (root / 'config.json').write_bytes(original)
            snapshot = self.snapshot(root)
            tampered = b'{"snapshot_repin":true}'
            (snapshot / 'config.json').write_bytes(tampered)
            manifest = json.loads((snapshot / 'inputs.json').read_text())
            for row in manifest['files']:
                if row['name'] == 'config.json':
                    row.update(length=len(tampered), sha256=sha(tampered))
            (snapshot / 'inputs.json').write_text(json.dumps(manifest))
            with self.assertRaises(ValueError):
                examples.export(root, metadata, Path(temp) / 'snapshot-bad')
            self.assertFalse((Path(temp) / 'snapshot-bad').exists())

    def test_rejects_tampered_snapshot_instead_of_falling_back_to_direct_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / 'fixture'
            metadata = self.fixture(root)
            snapshot = self.snapshot(root)
            (snapshot / 'config.json').write_text('{"tampered":true}')
            output = Path(temp) / 'bad'
            with self.assertRaises(ValueError):
                examples.export(root, metadata, output)
            self.assertFalse(output.exists())

    def test_current_graph_hash_selects_its_snapshot_without_overwriting_prior_inputs(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / 'fixture'
            metadata = self.fixture(root)
            first = self.snapshot(root)
            original_first = (first / 'config.json').read_bytes()
            graph = json.loads((root / 'graph.json').read_text())
            graph['provider_epoch'] = '05' * 32
            config = json.loads((root / 'config.json').read_text())
            config['provider_epoch'] = config['runtime_context']['provider_epoch'] = graph['provider_epoch']
            template = json.loads((root / 'provider-template.json').read_text())
            template['reset_epoch'] = graph['provider_epoch']
            for name, role, value in [('config.json', 'config', config), ('provider-template.json', 'provider_template', template)]:
                raw = json.dumps(value).encode()
                (root / name).write_bytes(raw)
                graph[role].update(length=len(raw), sha256=sha(raw))
            (root / 'graph.json').write_text(json.dumps(graph))
            second = self.snapshot(root)
            self.assertNotEqual(first, second)
            (root / 'config.json').write_text('{"provider_prepared":true}')
            output = Path(temp) / 'examples'
            examples.export(root, metadata, output)
            self.assertEqual((output / 'config.json').read_bytes(), (second / 'config.json').read_bytes())
            self.assertEqual((first / 'config.json').read_bytes(), original_first)

    def test_rejects_each_wrong_component_full_and_context_binding_before_output(self):
        mutations = [
            ('graph.json', ('core_manifest_digest',)),
            ('graph.json', ('integration_manifest_digest',)),
            ('graph.json', ('manifest_digest',)),
            ('config.json', ('core_manifest_digest',)),
            ('config.json', ('integration_manifest_digest',)),
            ('config.json', ('manifest_digest',)),
            ('config.json', ('integration_manifest', 'core_manifest_digest')),
            ('config.json', ('runtime_context', 'integration_manifest_digest')),
            ('config.json', ('runtime_context', 'manifest_digest')),
            ('config.json', ('runtime_context', 'binding')),
            ('launch.json', ('manifest_digest',)),
            ('provider-template.json', ('manifest_digest',)),
            ('provider-template.json', ('binding',)),
            ('provider-template.json', ('workspace_marker',)),
            ('provider-template.json', ('reset_epoch',)),
        ]
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            for index, (name, fields) in enumerate(mutations):
                with self.subTest(name=name, fields=fields):
                    root = base / ('fixture-' + str(index))
                    metadata = self.fixture(root)
                    value = json.loads((root / name).read_text())
                    target = value
                    for field in fields[:-1]:
                        target = target[field]
                    target[fields[-1]] = '09' * (16 if fields[-1] in ('binding', 'workspace_marker') else 32)
                    (root / name).write_text(json.dumps(value))
                    if name != 'graph.json':
                        # Keep the exact original-file pin valid: only semantic
                        # commitment/binding validation may reject this mutation.
                        graph = json.loads((root / 'graph.json').read_text())
                        role = {'config.json': 'config', 'launch.json': 'launch',
                                'provider-template.json': 'provider_template'}[name]
                        raw = (root / name).read_bytes()
                        graph[role].update(length=len(raw), sha256=sha(raw))
                        (root / 'graph.json').write_text(json.dumps(graph))
                    output = base / ('bad-' + str(index))
                    with self.assertRaisesRegex(ValueError, 'commitment|binding|epoch'):
                        examples.export(root, metadata, output)
                    self.assertFalse(output.exists())

    def test_rejects_substituted_artifact_metadata_and_existing_output(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            root = base / 'fixture'
            metadata = self.fixture(root)
            metadata['native_service_base']['sha256'] = 'e' * 64
            with self.assertRaises(ValueError):
                examples.export(root, metadata, base / 'bad')
            self.assertFalse((base / 'bad').exists())
            metadata['native_service_base']['sha256'] = sha(b'base')
            with self.assertRaises(FileExistsError):
                examples.export(root, metadata, root)

    def test_rejects_changed_original_and_symlinked_config_before_output(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            root = base / 'fixture'
            metadata = self.fixture(root)
            (root / 'target.bin').write_bytes(b'changed')
            with self.assertRaises(ValueError):
                examples.export(root, metadata, base / 'bad')
            (root / 'target.bin').write_bytes(b'target')
            config = root / 'config.json'
            actual = root / 'original-config'
            config.rename(actual)
            try:
                config.symlink_to(actual)
            except (OSError, NotImplementedError):
                self.skipTest('symlink creation unavailable')
            with self.assertRaises(ValueError):
                examples.export(root, metadata, base / 'bad2')
            self.assertFalse((base / 'bad2').exists())


if __name__ == '__main__':
    unittest.main()
