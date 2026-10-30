"""Host-only preparation boundary tests; dummy ELF bytes are never executed."""
import hashlib
import json
import pathlib
import subprocess
import shutil
import tempfile
import unittest
import shlex

ROOT = pathlib.Path(__file__).resolve().parents[2]
PREPARE = ROOT / 'examples/plugins/prepare.py'

class GuestVersionAdapter(unittest.TestCase):
    def test_length_ascii_and_process_exit_boundaries_with_explicit_synthetic_producer(self):
        # Only this tiny shell producer runs on the host, never a native client.
        # Exercise the actual adapter with disposable literal paths substituted.
        source = (ROOT / 'examples/plugins/fixture/client-version.sh').read_text()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            producer = root / 'synthetic-version-producer'
            adapter = root / 'adapter.sh'
            adapter.write_text(source.replace('/home/theseus/plugin-config', str(root))
                               .replace('/workspace/plugin-project', str(root))
                               .replace('/inputs/target/client/claude', shlex.quote(str(producer))))
            for output, status, accepted in [
                (b'a' * 128, 0, True),
                (b'a' * 128 + b'\n', 0, True),
                (b'a' * 129, 0, False),
                ('v\u00e9rsion'.encode(), 0, False),
                (b'2.1.312 (Claude Code)\n', 23, False),
            ]:
                with self.subTest(bytes=len(output), status=status, accepted=accepted):
                    (root / 'literal-output').write_bytes(output)
                    producer.write_text('#!/bin/sh\ncat ' + shlex.quote(str(root / 'literal-output')) + '\nexit ' + str(status) + '\n')
                    producer.chmod(0o755)
                    result = subprocess.run(['sh', str(adapter)], capture_output=True)
                    if accepted:
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertEqual(json.loads(result.stdout), {'version': 'a' * 128})
                    else:
                        self.assertNotEqual(result.returncode, 0)
                        self.assertEqual(result.stdout, b'')

class Preparation(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name)
        self.client = self.root / 'supplied client'
        self.data = b'\x7fELF' + bytes(range(256)) * 8193
        self.client.write_bytes(self.data)
        self.client.chmod(0o600)

    def prepare(self, output=None, client=None, scope='user', version='2.1.312 (Claude Code)', image='ubuntu:24.04'):
        return subprocess.run(['python3', str(PREPARE), '--client', str(client or self.client),
            '--client-version', version, '--scope', scope, '--output', str(output or self.root / 'output'),
            '--image', image], capture_output=True, text=True)

    def test_streamed_bytes_hash_permissions_and_exact_fixture_copy(self):
        fixture = ROOT / 'examples/plugins/fixture'
        before = {str(p.relative_to(fixture)): p.read_bytes() for p in fixture.rglob('*') if p.is_file()}
        result = self.prepare()
        self.assertEqual(result.returncode, 0, result.stderr)
        out = self.root / 'output'
        self.assertEqual((out / 'target/client/claude').read_bytes(), self.data)
        self.assertEqual((out / 'target/client/claude').stat().st_mode & 0o777, 0o755)
        manifest = json.loads((out / 'input-manifest.json').read_text())
        self.assertEqual(manifest['client']['sha256'], hashlib.sha256(self.data).hexdigest())
        self.assertEqual(manifest['client']['size'], len(self.data))
        self.assertEqual(manifest['client']['expected_version'], '2.1.312 (Claude Code)')
        self.assertEqual(manifest['client']['version_status'], 'expected until guest verification')
        self.assertEqual(manifest['provenance'], 'explicit supplied ELF; not executed on host')
        self.assertTrue(before)
        for relative, content in before.items():
            self.assertEqual((out / 'target' / relative).read_bytes(), content)
            self.assertEqual((fixture / relative).read_bytes(), content)
        self.assertEqual(self.client.read_bytes(), self.data)
        self.assertEqual(self.client.stat().st_mode & 0o777, 0o600)
        self.assertNotIn(str(self.root), (out / 'theseus.toml').read_text())

    def test_collision_does_not_remove_or_overwrite_existing_output(self):
        out = self.root / 'output'
        out.mkdir()
        (out / 'sentinel').write_text('keep')
        self.assertNotEqual(self.prepare().returncode, 0)
        self.assertEqual((out / 'sentinel').read_text(), 'keep')
        self.assertEqual(list(out.iterdir()), [out / 'sentinel'])

    def test_rejects_symlink_nonregular_non_elf_and_missing_input_atomically(self):
        link = self.root / 'link'
        link.symlink_to(self.client)
        script = self.root / 'shell'
        script.write_text('#!/bin/sh\nexit 0\n')
        for client in [link, self.root, script, self.root / 'absent']:
            with self.subTest(client=client):
                self.assertNotEqual(self.prepare(client=client).returncode, 0)
                self.assertFalse((self.root / 'output').exists())

    def test_output_in_fixture_tree_or_beneath_client_is_rejected(self):
        suffix = 'task3-preparation-check-' + self.root.name
        for out in [ROOT / 'examples/plugins/fixture' / suffix,
                    ROOT / 'examples/standalone' / suffix,
                    ROOT / 'tests/fixtures/plugins' / suffix,
                    ROOT / 'tests/support' / suffix,
                    self.client / 'nested']:
            with self.subTest(output=out):
                self.assertFalse(out.exists())
                # A regression may create this new known-owned test path; never
                # leave a staged ELF in a source tree after the red assertion.
                try:
                    self.assertNotEqual(self.prepare(output=out).returncode, 0)
                    self.assertFalse(out.exists())
                finally:
                    if out.is_dir():
                        shutil.rmtree(out)

    def test_invalid_template_values_leave_no_partial_output(self):
        for version in ['', 'bad"version', 'line\nbreak', 'x' * 129]:
            self.assertNotEqual(self.prepare(version=version).returncode, 0)
            self.assertFalse((self.root / 'output').exists())
        self.assertNotEqual(self.prepare(image='bad\nimage').returncode, 0)
        self.assertFalse((self.root / 'output').exists())

if __name__ == '__main__':
    unittest.main(verbosity=2)
