"""Portable owned guest-text/opaque-input oracle; never executes supplied clients."""
import hashlib
import json
import os
import runpy
from types import SimpleNamespace
from unittest.mock import patch
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]

class PortableGuestBytes(unittest.TestCase):
    def test_owned_checkout_scripts_have_scoped_lf_attributes_and_raw_lf(self):
        paths = []
        for tree in [ROOT / 'examples', ROOT / 'tests/fixtures']:
            for path in tree.rglob('*'):
                if path.is_file() and (path.suffix == '.sh' or path.name in ['sample-cli', 'comparison-cli', 'replay-client']):
                    paths.append(path.relative_to(ROOT).as_posix())
        self.assertGreater(len(paths), 50)
        for name in paths:
            result = subprocess.run(['git', 'check-attr', 'eol', '--', name], cwd=ROOT, capture_output=True, check=True)
            self.assertTrue(result.stdout.rstrip().endswith(b': eol: lf'), result.stdout)
            self.assertNotIn(b'\r\n', (ROOT / name).read_bytes(), name)

    def test_source_acquisition_rejects_nonregular_reparse_and_changed_identity(self):
        for kind in ['plugins', 'mcp']:
            acquire = runpy.run_path(str(ROOT / 'examples' / kind / 'prepare.py'))['open_client']
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                source = root / 'client'
                source.write_bytes(b'\x7fELF\r\n\x1a\x00\xff')
                with self.assertRaises((ValueError, OSError)):
                    acquire(root)
                alias = root / 'alias'
                if os.name == 'nt':
                    subprocess.run(['cmd.exe', '/D', '/C', 'mklink', '/J', str(alias), str(root)], check=True, capture_output=True)
                else:
                    alias.symlink_to(source)
                with self.assertRaises((ValueError, OSError)):
                    acquire(alias)
                if os.name == 'nt': alias.rmdir()
                original_fstat = os.fstat
                def changed(fd):
                    info = original_fstat(fd)
                    values = dict(st_mode=info.st_mode, st_dev=info.st_dev, st_ino=info.st_ino + 1)
                    if hasattr(info, 'st_file_attributes'): values['st_file_attributes'] = info.st_file_attributes
                    return SimpleNamespace(**values)
                with patch('os.fstat', side_effect=changed):
                    with self.assertRaisesRegex(ValueError, 'identity changed'):
                        acquire(source)
                if os.name == 'nt':
                    for bad in ['missing attributes', 'zero identity']:
                        def unavailable(fd):
                            info = original_fstat(fd)
                            values = dict(st_mode=info.st_mode, st_dev=info.st_dev, st_ino=info.st_ino)
                            if bad == 'zero identity': values.update(st_ino=0, st_file_attributes=info.st_file_attributes)
                            return SimpleNamespace(**values)
                        with patch('os.fstat', side_effect=unavailable):
                            with self.assertRaisesRegex(ValueError, 'attributes|identity is unavailable'):
                                acquire(source)
                with acquire(source) as stream:
                    self.assertEqual(stream.read(), b'\x7fELF\r\n\x1a\x00\xff')

    def test_prepared_variants_preserve_opaque_elf_and_emit_raw_lf(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            supplied = root / 'supplied-client'
            opaque = b'\x7fELF\r\n\x00\xff' + bytes(range(256)) * 257
            supplied.write_bytes(opaque)
            variants = [
                ('plugins', ['--scope', scope]) for scope in ['user', 'project', 'local']
            ] + [
                ('mcp', ['--adapter', adapter, '--delivery', delivery, '--scope', scope, '--scenario', scenario, '--protocol', protocol])
                for adapter, delivery, scope, scenario in [
                    ('claude-code-2.1.312', 'standalone', 'user', 'clean'),
                    ('claude-code-2.1.312', 'standalone', 'project', 'upgrade'),
                    ('claude-code-2.1.312', 'standalone', 'local', 'clean'),
                    ('claude-code-2.1.312', 'plugin', 'user', 'plugin_lifecycle'),
                    ('codex-cli-0.166.0', 'standalone', 'global', 'clean')]
                for protocol in ['2026-07-28', '2025-11-25']
            ]
            for index, (kind, options) in enumerate(variants):
                with self.subTest(kind=kind, options=options):
                    output = root / str(index)
                    version = 'codex-cli 0.166.0' if 'codex-cli-0.166.0' in options else '2.1.312 (Claude Code)'
                    result = subprocess.run([sys.executable, str(ROOT / 'examples' / kind / 'prepare.py'), '--client', str(supplied), '--client-version', version, '--output', str(output)] + options, capture_output=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual((output / 'target/client/claude').read_bytes(), opaque)
                    manifest = json.loads((output / 'input-manifest.json').read_bytes())
                    self.assertEqual(manifest['client']['sha256'], hashlib.sha256(opaque).hexdigest())
                    self.assertEqual(manifest['client']['size'], len(opaque))
                    scripts = []
                    for path in output.rglob('*'):
                        if path.is_file() and path.relative_to(output).as_posix() != 'target/client/claude':
                            raw = path.read_bytes()
                            raw.decode('utf-8')
                            self.assertNotIn(b'\r\n', raw, str(path))
                            if path.suffix == '.sh': scripts.append(raw)
                    self.assertGreaterEqual(len(scripts), 3)
                    action = (output / ('target/action.sh' if kind == 'mcp' else 'target/lifecycle.sh')).read_bytes()
                    self.assertIn(b'cp /inputs/target/client/claude "$client"', action)
                    self.assertIn(b'chmod 0755 "$client"', action)
                    self.assertIn(b'cmp /inputs/target/client/claude "$client"', action)
                    self.assertIn(b'client=/home/theseus/.local/bin/theseus-client', action)
            self.assertEqual(supplied.read_bytes(), opaque)

if __name__ == '__main__':
    unittest.main(verbosity=2)
