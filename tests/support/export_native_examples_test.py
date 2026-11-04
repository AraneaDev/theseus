"""Read-only example packaging controls; no target or application is started."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('examples', Path(__file__).with_name('export_native_examples.py'))
examples = importlib.util.module_from_spec(spec)
spec.loader.exec_module(examples)


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


class ExportExamples(unittest.TestCase):
    def fixture(self, root, os_name='linux'):
        root.mkdir()
        keys = {'base': 'native_service_base', 'target': 'native_service_target',
                'controller': 'native_integration_controller', 'helper': 'helper_production',
                'provider': 'native_service_fixture'}
        graph = {'kind': 'theseus_native_integration_fixture_graph', 'schema': 1,
                 'manifest_digest': 'a' * 64, 'integration_manifest_digest': 'b' * 64}
        metadata = {'source_sha': 'c' * 40, 'source_tree_sha256': 'd' * 64}
        for role, key in keys.items():
            path = root / (role + '.bin')
            path.write_bytes(role.encode())
            pin = {'path': str(path), 'sha256': sha(path.read_bytes()), 'length': path.stat().st_size}
            graph[role] = pin
            metadata[key] = pin.copy()
        recipe = {'kind': 'theseus_native_recipe', 'schema': 2, 'integration_manifest_digest': 'b' * 64,
                  'catalog': {'os': os_name, 'manifest_digest': 'a' * 64,
                              'base': {k: graph['base'][k] for k in ('length', 'sha256')},
                              'target': {k: graph['target'][k] for k in ('length', 'sha256')}}}
        files = {'recipe.json': recipe, 'config.json': {'schema': 1},
                 'launch.json': {'fixture': 'explicit'}, 'provider-template.json': {'schema': 1}}
        for name, value in files.items():
            path = root / name
            path.write_text(json.dumps(value))
            if name != 'recipe.json':
                role = {'config.json': 'config', 'launch.json': 'launch', 'provider-template.json': 'provider_template'}[name]
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
