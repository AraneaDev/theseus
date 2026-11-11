"""Behavioral checks for the shared local/CI validation driver (no Cargo/Docker)."""
import copy
import importlib.util
import io
import json
import os
import pathlib
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from unittest.mock import patch

sys.dont_write_bytecode = True

SCRIPT = pathlib.Path(__file__).resolve().parents[2] / 'scripts' / 'validate.py'
spec = importlib.util.spec_from_file_location('validation', SCRIPT)
validation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(validation)


class StageRunner:
    def __init__(self, evidence, env, failure=None):
        self.evidence = evidence
        self.env = dict(env)
        self.failure = failure
        self.calls = []

    def cargo(self, name, *arguments, **kwargs):
        self.calls.append((name, dict(self.env)))
        if name.startswith('early-') and self.failure is not None:
            raise self.failure


class ValidationTests(unittest.TestCase):
    def catalog_graph(self):
        return [{'reason': 'compiler-artifact', 'target': {'name': name, 'kind': [kind]},
                 'features': ['native-catalog-test-support'] if name == 'theseus' or kind == 'bin' else []}
                for name, kind in [('theseus_host', 'lib'), ('theseus_native_helper_store', 'lib'),
                                   ('theseus_native_client', 'lib'), ('theseus', 'lib')] +
                                  [(target, 'bin') for target, _ in validation.CATALOG_ARTIFACTS.values()]]

    def add_catalog_metadata(self, root, metadata):
        graph = self.catalog_graph()
        metadata['native_catalog_graph'] = graph
        for key, (target, _) in validation.CATALOG_ARTIFACTS.items():
            path = root / key
            path.write_bytes(key.encode())
            path.chmod(0o755)
            artifact = next(item for item in graph if item['target']['name'] == target)
            metadata[key] = {'path': str(path), 'sha256': validation.sha256(path), 'cargo_artifact': artifact}

        graph = [{'reason': 'compiler-artifact', 'target': {'name': target, 'kind': ['bin']},
                  'features': ['native-integration-test-support']}
                 for target, _ in validation.INTEGRATION_ARTIFACTS.values()]
        metadata['native_integration_graph'] = graph
        for key, (target, _) in validation.INTEGRATION_ARTIFACTS.items():
            path = root / key
            path.write_bytes(key.encode())
            path.chmod(0o755)
            artifact = next(item for item in graph if item['target']['name'] == target)
            metadata[key] = {'path': str(path), 'sha256': validation.sha256(path), 'cargo_artifact': artifact}

        for key, fixture in (('desktop_broker_production', False), ('desktop_broker_fixture', True)):
            path = root / key
            path.write_bytes(key.encode())
            path.chmod(0o755)
            artifact = {'reason': 'compiler-artifact', 'target': {'name': 'theseus-native-desktop-broker', 'kind': ['bin']}, 'features': ['test-support'] if fixture else []}
            metadata[key] = {'path': str(path), 'sha256': validation.sha256(path), 'cargo_artifact': artifact}
            metadata[key + '_graph'] = [artifact, *[{'target': {'name': name}, 'features': [feature] if fixture else []} for name, feature in [('theseus_host', 'desktop-test-support'), ('theseus_native_protocol', 'test-support')]]]

    def test_cp1252_console_preserves_raw_utf8_and_required_case_receipt(self):
        raw = ('running 1 test\ntest required_case ... native \U0001f600\nok\n'
               'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n').encode('utf-8')
        diagnostic = 'stderr caf\u00e9 \U0001f600\n'.encode('utf-8')
        with tempfile.TemporaryDirectory() as temp, io.TextIOWrapper(io.BytesIO(), encoding='cp1252', errors='strict') as console:
            runner = validation.Runner(pathlib.Path(temp), pathlib.Path.cwd())
            command = [sys.executable, '-c',
                       f'import sys; sys.stdout.buffer.write(bytes.fromhex({raw.hex()!r})); sys.stdout.buffer.flush(); '
                       f'sys.stderr.buffer.write(bytes.fromhex({diagnostic.hex()!r}))']
            with redirect_stdout(console):
                try:
                    output = runner.run('unicode-cargo', command, required_cases=['required_case'], require_tests=True)
                except UnicodeEncodeError as error:
                    self.fail(f'Console encoding changed a successful required-case result: {error}')
            console.flush()
            self.assertEqual((runner.evidence / 'unicode-cargo.log').read_bytes(), raw + diagnostic)
            self.assertEqual(output, (raw + diagnostic).decode('utf-8'))
            receipt = json.loads((runner.evidence / 'unicode-cargo.result.json').read_text())
            self.assertTrue(receipt['pass'])
            self.assertEqual(receipt['status'], 'returned')
            self.assertEqual(receipt['executed_cases'], ['required_case'])
            self.assertTrue(receipt['test_execution_verified'])
            rendered = console.buffer.getvalue().decode('cp1252')
            self.assertIn('native \\U0001f600', rendered)
            self.assertIn('stderr caf\u00e9 \\U0001f600', rendered)
            self.assertEqual((console.encoding, console.errors), ('cp1252', 'strict'))

    def test_cp1252_console_preserves_stdout_only_and_stderr_artifacts(self):
        output = '/owned/fixture-\U0001f600\n'.encode('utf-8')
        diagnostic = 'compiler \U0001f600\n'.encode('utf-8')
        with tempfile.TemporaryDirectory() as temp, io.TextIOWrapper(io.BytesIO(), encoding='cp1252', errors='strict') as console:
            runner = validation.Runner(pathlib.Path(temp), pathlib.Path.cwd())
            command = [sys.executable, '-c',
                       f'import sys; sys.stdout.buffer.write(bytes.fromhex({output.hex()!r})); '
                       f'sys.stderr.buffer.write(bytes.fromhex({diagnostic.hex()!r}))']
            with redirect_stdout(console):
                try:
                    returned = runner.run('unicode-fixture', command, stdout_only=True)
                except UnicodeEncodeError as error:
                    self.fail(f'Console encoding changed the fixture stdout contract: {error}')
            self.assertEqual(returned, output.decode('utf-8'))
            self.assertEqual((runner.evidence / 'unicode-fixture.stdout').read_bytes(), output)
            self.assertEqual((runner.evidence / 'unicode-fixture.log').read_bytes(), diagnostic)
            self.assertTrue(json.loads((runner.evidence / 'unicode-fixture.result.json').read_text())['pass'])

    def test_cp1252_console_preserves_unicode_evidence_path_and_failure_receipt(self):
        with tempfile.TemporaryDirectory() as temp, \
                io.TextIOWrapper(io.BytesIO(), encoding='cp1252', errors='strict') as stdout, \
                io.TextIOWrapper(io.BytesIO(), encoding='cp1252', errors='strict') as stderr:
            evidence = pathlib.Path(temp) / 'evidence-\U0001f600'
            message = 'native failure \U0001f600'
            with patch.object(sys, 'argv', ['validate.py', 'native', '--evidence', str(evidence)]), \
                    patch.object(validation, 'source_identity', side_effect=validation.GateFailure(message)), \
                    redirect_stdout(stdout), redirect_stderr(stderr):
                try:
                    code = validation.main()
                except UnicodeEncodeError as error:
                    self.fail(f'Console encoding replaced the original failure: {error}')
            stdout.flush()
            stderr.flush()
            self.assertEqual(code, 1)
            receipt = json.loads((evidence / 'native-all.result.json').read_text())
            self.assertFalse(receipt['pass'])
            self.assertEqual(receipt['error'], message)
            self.assertIn('evidence-\\U0001f600', stdout.buffer.getvalue().decode('cp1252'))
            self.assertIn('native failure \\U0001f600', stderr.buffer.getvalue().decode('cp1252'))
            self.assertEqual((stdout.encoding, stdout.errors), ('cp1252', 'strict'))
            self.assertEqual((stderr.encoding, stderr.errors), ('cp1252', 'strict'))

    def test_complete_run_removes_only_builder_named_read_only_artifact_tree(self):
        with tempfile.TemporaryDirectory() as evidence_root, tempfile.TemporaryDirectory() as temporary:
            evidence = pathlib.Path(evidence_root)
            temporary = pathlib.Path(temporary).resolve()

            def pinned(root):
                binary = root / 'fixture' / 'theseus-host-fixture'
                binary.parent.mkdir(parents=True)
                binary.write_bytes(b'fixture')
                (evidence / 'native-artifacts.json').write_text(json.dumps({'fixture': {'path': str(binary)}}))
                return binary

            with patch.object(validation.tempfile, 'gettempdir', return_value=str(temporary)):
                self.assertEqual(validation.remove_pinned_artifacts(evidence), 'absent')
                owned = temporary / 'theseus-native-artifacts-abc'
                binary = pinned(owned)
                if os.name != 'nt':
                    os.chmod(binary.parent, 0o500)
                self.assertEqual(validation.remove_pinned_artifacts(evidence), f'removed {owned}')
                self.assertFalse(owned.exists())
                other = temporary / 'unrelated'
                pinned(other)
                self.assertTrue(validation.remove_pinned_artifacts(evidence).startswith('refused'))
                self.assertTrue(other.exists())
                nested = temporary / 'nested' / 'theseus-native-artifacts-def'
                pinned(nested)
                self.assertTrue(validation.remove_pinned_artifacts(evidence).startswith('refused'))
                self.assertTrue(nested.exists())

    def test_early_trace_is_scoped_before_later_runtime(self):
        key = 'THESEUS_TERMINAL_TRACE'
        for stage in ('early-controls', 'early-cli'):
            for original in ({}, {key: 'caller-value'}, {key: ''}):
                with self.subTest(stage=stage, original=original), tempfile.TemporaryDirectory() as temp:
                    runner = StageRunner(pathlib.Path(temp), original)
                    with patch.object(validation, 'load_pinned', return_value={}):
                        validation.native(runner, stage)
                        self.assertEqual(runner.calls[0][1][key], '1')
                        self.assertEqual(key in runner.env, key in original)
                        self.assertEqual(runner.env.get(key), original.get(key))
                        validation.native(runner, 'runtime')
                    self.assertEqual([name for name, _ in runner.calls[1:]],
                                     ['workspace-tests', 'native-cli-terminal', 'native-explicit-terminal-controls', 'native-helper-runtime', 'native-client-runtime', 'native-profile-runtime', 'native-catalog-library', 'native-catalog-adapter', 'native-selected', 'native-recovery', 'native-consumers'])
                    for _, env in runner.calls[1:]:
                        self.assertEqual(key in env, key in original)
                        self.assertEqual(env.get(key), original.get(key))

    def test_early_trace_restores_caller_environment_when_stage_fails(self):
        key = 'THESEUS_TERMINAL_TRACE'
        for stage in ('early-controls', 'early-cli'):
            for original in ({}, {key: 'caller-value'}, {key: ''}):
                with self.subTest(stage=stage, original=original), tempfile.TemporaryDirectory() as temp:
                    failure = validation.GateFailure('deliberate early-stage failure')
                    runner = StageRunner(pathlib.Path(temp), original, failure)
                    with patch.object(validation, 'load_pinned', return_value={}):
                        with self.assertRaises(validation.GateFailure) as caught:
                            validation.native(runner, stage)
                    self.assertIs(caught.exception, failure)
                    self.assertEqual(runner.calls[0][1][key], '1')
                    self.assertEqual(key in runner.env, key in original)
                    self.assertEqual(runner.env.get(key), original.get(key))

    def test_failed_desktop_step_retains_bounded_printed_fixture_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            evidence = root / 'evidence'
            evidence.mkdir()
            fixture = root / 'fixture'
            (fixture / '.theseus-runs/native-1').mkdir(parents=True)
            (fixture / '.theseus-runs/native-1/native-report.json').write_text('{"error":"provider"}')
            (fixture / 'big.bin').write_bytes(b'x' * 64)
            if hasattr(os, 'mkfifo'):
                os.mkfifo(fixture / 'pipe')
            outside = root / 'outside.txt'
            outside.write_text('secret')
            try:
                (fixture / 'link').symlink_to(outside)
            except (OSError, NotImplementedError):
                pass
            (evidence / 'source-desktop-product.log').write_text(
                # libtest --nocapture prints the test name on the same line.
                f'test source_desktop_x ... retained desktop fixture: {fixture}\r\nretained desktop endpoints: {root / "absent"}\nretained desktop fixture: relative\n')
            result = validation.retain_desktop_evidence(evidence, 'source-desktop-product', file_limit=32)
            copied = evidence / 'retained-desktop/source-desktop-product/0/dot.theseus-runs/native-1/native-report.json'
            self.assertEqual(copied.read_text(), '{"error":"provider"}')
            self.assertFalse((evidence / 'retained-desktop/source-desktop-product/0/big.bin').exists())
            self.assertFalse((evidence / 'retained-desktop/source-desktop-product/0/link').exists())
            self.assertTrue((evidence / 'retained-desktop/source-desktop-product/manifest.json').is_file())
            self.assertTrue(any('skipped invalid' in note for note in result['notes']))
            self.assertEqual(validation.retain_desktop_evidence(evidence, 'missing-step')['copied'], [])

    def test_zero_tests_cannot_pass_required_cargo_gate(self):
        self.assert_required_cases_rejected([], ['required_case'])

    def test_partial_required_inventory_cannot_pass(self):
        self.assert_required_cases_rejected(['first_case'], ['first_case', 'second_case'])

    def test_nonempty_unrelated_case_cannot_replace_required_case(self):
        self.assert_required_cases_rejected(['unrelated_case'], ['required_case'])

    def test_named_required_case_passes_through_actual_runner(self):
        receipt = self.cargo_transcript(['required_case'], ['required_case'])
        self.assertTrue(receipt['pass'])
        self.assertEqual(receipt['executed_cases'], ['required_case'])

    def test_nocapture_diagnostics_before_case_ok_are_attributed(self):
        receipt = self.cargo_transcript(['required_case'], ['required_case'], diagnostics=True)
        self.assertTrue(receipt['pass'])
        self.assertEqual(receipt['executed_cases'], ['required_case'])

    def cargo_transcript(self, selected, required, diagnostics=False, reject=False):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            fake_cargo = root / 'fake_cargo.py'
            lines = [f'running {len(selected)} tests']
            for name in selected:
                lines.extend([f'test {name} ... native diagnostic', 'ok'] if diagnostics else [f'test {name} ... ok'])
            lines.append(f'test result: ok. {len(selected)} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s')
            fake_cargo.write_text('print(' + repr('\n'.join(lines)) + ')', encoding='utf-8')
            runner = validation.Runner(root / 'evidence', pathlib.Path.cwd())
            command = [sys.executable, str(fake_cargo)]
            if reject:
                with self.assertRaises(validation.GateFailure):
                    runner.run('required-cargo', command, required_cases=required, require_tests=True)
            else:
                runner.run('required-cargo', command, required_cases=required, require_tests=True)
            return json.loads((runner.evidence / 'required-cargo.result.json').read_text())

    def assert_required_cases_rejected(self, selected, required):
        receipt = self.cargo_transcript(selected, required, reject=True)
        self.assertFalse(receipt['pass'])
        self.assertEqual(receipt['exit_code'], 0)
        self.assertEqual(receipt['status'], 'required_tests_missing')

    def test_nonzero_command_is_recorded_and_fails_closed(self):
        with tempfile.TemporaryDirectory() as temp:
            runner = validation.Runner(pathlib.Path(temp), pathlib.Path.cwd())
            with self.assertRaises(validation.GateFailure):
                runner.run('deliberate-failure', [sys.executable, '-c', 'print("receipt-output"); raise SystemExit(7)'])
            receipt = json.loads((pathlib.Path(temp) / 'deliberate-failure.result.json').read_text())
            self.assertEqual(receipt['exit_code'], 7)
            self.assertFalse(receipt['pass'])
            self.assertIn('receipt-output', (pathlib.Path(temp) / 'deliberate-failure.log').read_text())

    def test_fixture_stdout_path_is_not_mixed_with_compiler_diagnostics(self):
        with tempfile.TemporaryDirectory() as temp:
            runner = validation.Runner(pathlib.Path(temp), pathlib.Path.cwd())
            value = runner.run('stdout-contract', [sys.executable, '-c',
                'import sys; print("compiler-diagnostic",file=sys.stderr); print("/owned/fixture")'], stdout_only=True)
            self.assertEqual(value.strip(), '/owned/fixture')
            self.assertIn('compiler-diagnostic', (pathlib.Path(temp) / 'stdout-contract.log').read_text())

    def test_pinned_production_survives_later_feature_build_overwrite(self):
        builder_spec = importlib.util.spec_from_file_location('fixture_builder', SCRIPT.parents[1] / 'tests/support/build_host_fixture.py')
        builder = importlib.util.module_from_spec(builder_spec)
        builder_spec.loader.exec_module(builder)
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            source = root / 'theseus'
            source.write_bytes(b'production-without-test-features')
            metadata = builder.pin(root, [{'target': {'name': 'theseus'}, 'executable': str(source)}], 'theseus', 'production')
            source.write_bytes(b'later-test-feature-build')
            self.assertEqual(pathlib.Path(metadata['path']).read_bytes(), b'production-without-test-features')
            self.assertEqual(validation.sha256(pathlib.Path(metadata['path'])), metadata['sha256'])

    def test_timeout_is_failure_and_returned_child_is_reaped(self):
        with tempfile.TemporaryDirectory() as temp:
            runner = validation.Runner(pathlib.Path(temp), pathlib.Path.cwd())
            with self.assertRaises(validation.GateFailure):
                runner.run('watchdog', [sys.executable, '-c', 'import time; time.sleep(60)'], timeout=0.05)
            receipt = json.loads((pathlib.Path(temp) / 'watchdog.result.json').read_text())
            self.assertEqual(receipt['exit_code'], 124)
            self.assertEqual(receipt['status'], 'watchdog_failure')
            self.assertFalse(receipt['pass'])

    def test_missing_executable_is_failure_receipt(self):
        with tempfile.TemporaryDirectory() as temp:
            runner = validation.Runner(pathlib.Path(temp), pathlib.Path.cwd())
            with self.assertRaises(validation.GateFailure):
                runner.run('missing-tool', [str(pathlib.Path(temp) / 'absent-executable')])
            receipt = json.loads((pathlib.Path(temp) / 'missing-tool.result.json').read_text())
            self.assertEqual(receipt['status'], 'spawn_failure')
            self.assertFalse(receipt['pass'])

    def test_artifact_tamper_and_missing_fixture_are_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            artifact = root / 'application'
            artifact.write_bytes(b'trusted')
            artifact.chmod(0o755)
            metadata = {'fixture': {'path': str(artifact), 'sha256': validation.sha256(artifact)},
                        'application': {'path': str(artifact), 'sha256': validation.sha256(artifact)}}
            metadata['production'] = dict(metadata['application'])
            metadata['mcp_fixture'] = dict(metadata['fixture'])
            for key in ('helper_production', 'helper_fixture'):
                path = root / key
                path.write_bytes(key.encode())
                path.chmod(0o755)
                metadata[key] = {'path': str(path), 'sha256': validation.sha256(path)}
            self.add_catalog_metadata(root, metadata)
            validation.verify_artifacts(metadata)
            artifact.write_bytes(b'replaced')
            with self.assertRaises(validation.GateFailure):
                validation.verify_artifacts(metadata)
            artifact.unlink()
            with self.assertRaises(validation.GateFailure):
                validation.verify_artifacts(metadata)



    def test_integration_metadata_rejects_missing_changed_aliased_and_fault_graph(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            metadata = {}
            for key in ('fixture', 'application', 'production', 'mcp_fixture', 'helper_production', 'helper_fixture'):
                path = root / key
                path.write_bytes(key.encode())
                path.chmod(0o755)
                metadata[key] = {'path': str(path), 'sha256': validation.sha256(path)}
            self.add_catalog_metadata(root, metadata)
            validation.verify_artifacts(metadata)
            for key in validation.INTEGRATION_ARTIFACTS:
                bad = json.loads(json.dumps(metadata))
                del bad[key]
                with self.subTest(missing=key), self.assertRaises(validation.GateFailure):
                    validation.verify_artifacts(bad)
            for change in ('graph', 'artifact', 'feature', 'alias', 'fault', 'malformed'):
                bad = json.loads(json.dumps(metadata))
                if change == 'graph':
                    del bad['native_integration_graph']
                elif change == 'artifact':
                    bad['native_service_base']['cargo_artifact'] = bad['native_service_target']['cargo_artifact']
                elif change == 'feature':
                    bad['native_integration_graph'][0]['features'] = []
                elif change == 'alias':
                    for field in ('path', 'sha256'):
                        bad['native_service_base'][field] = bad['native_service_target'][field]
                elif change == 'fault':
                    bad['native_integration_graph'].append({'target': {'name': 'theseus_host'}, 'features': ['test-support']})
                else:
                    bad['native_integration_graph'].append(None)
                with self.subTest(change=change), self.assertRaises(validation.GateFailure):
                    validation.verify_artifacts(bad)
            pathlib.Path(metadata['native_service_base']['path']).write_bytes(b'replaced')
            with self.assertRaises(validation.GateFailure):
                validation.verify_artifacts(metadata)

    def test_broker_artifact_graphs_keep_production_and_source_features_separate(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            metadata = {}
            for key in ('fixture', 'application', 'production', 'mcp_fixture', 'helper_production', 'helper_fixture'):
                path = root / key
                path.write_bytes(key.encode())
                path.chmod(0o755)
                metadata[key] = {'path': str(path), 'sha256': validation.sha256(path)}
            self.add_catalog_metadata(root, metadata)
            validation.verify_artifacts(metadata)
            for change in ('missing', 'target', 'source', 'host', 'protocol', 'alias'):
                bad = json.loads(json.dumps(metadata))
                if change == 'missing':
                    del bad['desktop_broker_fixture_graph']
                elif change == 'target':
                    bad['desktop_broker_fixture']['cargo_artifact']['target']['name'] = 'theseus-native-helper-fixture'
                elif change == 'source':
                    bad['desktop_broker_fixture']['cargo_artifact']['features'] = []
                    bad['desktop_broker_fixture_graph'][0]['features'] = []
                elif change in ('host', 'protocol'):
                    index = 1 if change == 'host' else 2
                    feature = 'desktop-test-support' if change == 'host' else 'test-support'
                    bad['desktop_broker_production_graph'][index]['features'] = [feature]
                else:
                    for field in ('path', 'sha256'):
                        bad['desktop_broker_production'][field] = bad['desktop_broker_fixture'][field]
                with self.subTest(change=change), self.assertRaises(validation.GateFailure):
                    validation.verify_artifacts(bad)

    def test_nonexecutable_pinned_helper_is_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            metadata = {}
            for key in ('fixture', 'application', 'production', 'mcp_fixture', 'helper_production', 'helper_fixture'):
                path = root / key
                path.write_bytes(key.encode())
                path.chmod(0o755)
                metadata[key] = {'path': str(path), 'sha256': validation.sha256(path)}
            self.add_catalog_metadata(root, metadata)
            validation.verify_artifacts(metadata)
            (root / 'helper_production').chmod(0o644)
            if sys.platform == 'win32':
                # Windows launchability is not represented by Unix execute bits.
                validation.verify_artifacts(metadata)
            else:
                with self.assertRaises(validation.GateFailure):
                    validation.verify_artifacts(metadata)

    def test_builder_pins_helper_before_feature_overwrite_and_exports_owned_metadata(self):
        builder_spec = importlib.util.spec_from_file_location('fixture_builder', SCRIPT.parents[1] / 'tests/support/build_host_fixture.py')
        builder = importlib.util.module_from_spec(builder_spec)
        builder_spec.loader.exec_module(builder)
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            owned = root / 'owned'
            owned.mkdir()
            cargo = root / 'cargo'
            cargo.mkdir()
            def graph(names, fixture=False, host_support=False, catalog=False, integration=False):
                artifacts = []
                for name in names:
                    path = cargo / name
                    path.write_bytes((name + ('-fixture' if fixture else '-default')).encode())
                    path.chmod(0o755)
                    kind = 'lib' if '_' in name or ((catalog or integration) and name == 'theseus') else 'bin'
                    artifacts.append({'reason': 'compiler-artifact', 'target': {'name': name, 'kind': [kind]},
                                      'features': ['desktop-test-support'] if fixture and name == 'theseus_host' else
                                      ['test-support'] if (host_support and name == 'theseus_host') or
                                      (fixture and name in ('theseus_native_helper', 'theseus-native-helper-fixture', 'theseus-native-desktop-broker', 'theseus_native_protocol')) else
                                      ['native-catalog-test-support'] if catalog and (name == 'theseus' or kind == 'bin') else
                                      ['native-integration-test-support'] if integration and (name == 'theseus' or kind == 'bin') else [],
                                      'executable': str(path) if kind == 'bin' else None})
                return artifacts
            def fake_build(args):
                if args == ['--bin', 'theseus']:
                    return graph(('theseus_host', 'theseus'))
                if args == ['-p', 'theseus-native-helper', '--bin', 'theseus-native-helper', '--bin', 'theseus-native-desktop-broker']:
                    return graph(('theseus_host', 'theseus_native_protocol', 'theseus_native_helper', 'theseus-native-helper', 'theseus-native-desktop-broker'))
                if args == ['--workspace', '--bins', '--features', 'theseus-host/test-support']:
                    (cargo / 'theseus-native-helper').write_bytes(b'overwritten-feature-unified-helper')
                    return graph(('theseus_host', 'theseus-host-fixture', 'theseus'), host_support=True)
                if args == ['--features', 'mcp-test-support', '--bin', 'theseus-mcp-fixture']:
                    return graph(('theseus-mcp-fixture',))
                if args == ['-p', 'theseus-native-helper', '--features', 'test-support', '--bin', 'theseus-native-helper-fixture', '--bin', 'theseus-native-desktop-broker']:
                    return graph(('theseus_host', 'theseus_native_protocol', 'theseus_native_helper', 'theseus-native-helper-fixture', 'theseus-native-desktop-broker'), fixture=True)
                if args == ['--features', 'native-catalog-test-support', '--bin', 'theseus-native-catalog-fixture', '--bin', 'theseus-native-base-reader', '--bin', 'theseus-native-target-reader']:
                    return graph(('theseus_host', 'theseus_native_helper_store', 'theseus_native_client', 'theseus', 'theseus-native-catalog-fixture', 'theseus-native-base-reader', 'theseus-native-target-reader'), catalog=True)
                if args == ['--features', 'native-integration-test-support', *[part for target, _ in validation.INTEGRATION_ARTIFACTS.values() for part in ['--bin', target]]]:
                    return graph(('theseus_host', 'theseus', *[target for target, _ in validation.INTEGRATION_ARTIFACTS.values()]), integration=True)
                if args == ['--features', 'native-integration-test-support', '--no-run', *[part for target in validation.INTEGRATION_TEST_ARTIFACTS for part in ['--test', target]]]:
                    artifacts = graph(validation.INTEGRATION_TEST_ARTIFACTS, integration=True)
                    for artifact in artifacts:
                        artifact['target']['kind'] = ['test']
                        artifact['features'] = ['native-integration-test-support']
                        artifact['executable'] = str(cargo / artifact['target']['name'])
                    return artifacts
                self.fail(f'unexpected build: {args}')
            output = io.StringIO()
            with patch.object(builder, 'build', side_effect=fake_build), \
                    patch.object(builder.tempfile, 'mkdtemp', return_value=str(owned)), \
                    patch.object(builder.VALIDATION, 'source_identity', return_value={'source_sha': 'test-source'}), \
                    redirect_stdout(output):
                builder.main()
            metadata = json.loads((owned / 'artifacts.json').read_text())
            normal = pathlib.Path(metadata['helper_production']['path'])
            self.assertEqual(normal.read_bytes(), b'theseus-native-helper-default')
            self.assertEqual(metadata['helper_production']['cargo_artifact']['target']['name'], 'theseus-native-helper')
            validation.check_helper_graph(metadata['helper_production_graph'])
            validation.check_helper_graph(metadata['helper_fixture_graph'], fixture=True)
            self.assertEqual(json.loads((pathlib.Path(output.getvalue().strip()).parent / 'artifacts.json').read_text()), metadata)
            env_file = root / 'github-env'
            runner = validation.Runner(root / 'evidence')
            with patch.object(runner, 'run', return_value=output.getvalue()), \
                    patch.dict(validation.os.environ, {'GITHUB_ENV': str(env_file)}):
                validation.fixture(runner)
            for key, variable in (('helper_production', 'THESEUS_NATIVE_HELPER_BINARY'),
                                  ('helper_fixture', 'THESEUS_NATIVE_HELPER_FIXTURE'),
                                  *[(key, variable) for key, (_, variable) in (validation.CATALOG_ARTIFACTS | validation.INTEGRATION_ARTIFACTS).items()]):
                self.assertEqual(runner.env[variable], metadata[key]['path'])
                self.assertIn(f'{variable}={metadata[key]["path"]}\n', env_file.read_text())
            self.assertEqual(json.loads((runner.evidence / 'native-artifacts.json').read_text()), metadata)

    def test_native_helper_gate_requires_actual_host_inventory_and_exact_target(self):
        class RecordingRunner(StageRunner):
            def cargo(inner, name, *args, **kwargs):
                inner.calls.append((name, args, kwargs))
            def run(inner, *args, **kwargs):
                pass
        common = ['explicit_config_and_wrong_lease_file_requests_never_create_authority',
                  'ordinary_pipes_prove_commit_before_ack_spawn_and_finished',
                  'lost_acquire_reply_reuses_committed_intent_and_lease',
                  'cached_cancel_remains_responsive_under_saturated_output',
                  'finished_is_durable_before_optional_output_drain',
                  'native_child_status_and_probe_principal_are_observed_truthfully',
                  'eof_and_unresolved_cleanup_keep_owners_until_join_or_exit_two',
                  'fixture_faults_are_absent_from_normal_helper_graph',
                  'native_leaf_files_observe_actual_content_empty_limit_and_ranges',
                  'native_leaf_files_exclusive_create_quarantines_and_restarts_exact_reason',
                  'native_leaf_files_wrong_closed_lease_and_run_busy_preserve_workspace',
                  'native_leaf_files_replay_conflict_and_run_generations_retire',
                  'native_leaf_files_commit_ordering_and_cached_control_at_worker_gates',
                  'native_leaf_files_real_write_failure_quarantines_and_restarts']
        for system in ('Linux', 'Darwin', 'Windows'):
            with tempfile.TemporaryDirectory() as temp:
                runner = RecordingRunner(pathlib.Path(temp), {})
                with patch.object(validation, 'load_pinned', return_value={}), \
                        patch.object(validation.platform, 'system', return_value=system):
                    validation.native(runner, 'precompile')
                    validation.native(runner, 'runtime')
                calls = {name: (args, kwargs) for name, args, kwargs in runner.calls}
                prefix = ('test', '--locked', '-p', 'theseus-native-helper', '--features', 'test-support', '--test', 'fixture_runtime')
                self.assertEqual(calls['native-helper-precompile'], (prefix + ('--no-run',), {'timeout': 180}))
                expected = common + (['windows_unsigned_high_bit_exit_status_is_preserved'] if system == 'Windows' else [])
                self.assertEqual(calls['native-helper-runtime'], (prefix + ('--', '--nocapture', '--test-threads=1'),
                                                                 {'timeout': 240, 'required_cases': expected}))

    def helper_graph(self, fixture=False):
        feature = ['test-support'] if fixture else []
        return [{'reason': 'compiler-artifact', 'target': {'name': name},
                 'features': feature if name != 'theseus_host' else []}
                for name in ('theseus_host', 'theseus_native_helper',
                             'theseus-native-helper-fixture' if fixture else 'theseus-native-helper')]

    def test_helper_graph_rejects_missing_or_feature_unified_artifacts(self):
        for fixture in (False, True):
            good = self.helper_graph(fixture)
            validation.check_helper_graph(good, fixture=fixture)
            for index in range(3):
                with self.subTest(fixture=fixture, missing=index):
                    with self.assertRaises(validation.GateFailure):
                        validation.check_helper_graph(good[:index] + good[index + 1:], fixture=fixture)
            for index in range(3):
                bad = json.loads(json.dumps(good))
                bad[index]['features'] = [] if bad[index]['features'] else ['test-support']
                with self.subTest(fixture=fixture, feature=index):
                    with self.assertRaises(validation.GateFailure):
                        validation.check_helper_graph(bad, fixture=fixture)
        with self.assertRaises(validation.GateFailure):
            validation.check_helper_graph(self.helper_graph() + self.helper_graph(True)[-1:])

    def test_missing_tampered_or_substituted_normal_helper_fails_closed(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            metadata = {}
            for key in ('fixture', 'application', 'production', 'mcp_fixture', 'helper_production', 'helper_fixture'):
                path = root / key
                path.write_bytes(key.encode())
                path.chmod(0o755)
                metadata[key] = {'path': str(path), 'sha256': validation.sha256(path)}
            self.add_catalog_metadata(root, metadata)
            validation.verify_artifacts(metadata)
            normal = metadata.pop('helper_production')
            with self.assertRaises(validation.GateFailure):
                validation.verify_artifacts(metadata)
            metadata['helper_production'] = dict(metadata['helper_fixture'])
            with self.assertRaises(validation.GateFailure):
                validation.verify_artifacts(metadata)
            metadata['helper_production'] = normal
            pathlib.Path(normal['path']).write_bytes(b'tampered')
            with self.assertRaises(validation.GateFailure):
                validation.verify_artifacts(metadata)

    def test_helper_release_and_fixture_graph_checks_are_both_required(self):
        class GraphRunner:
            def __init__(inner):
                inner.calls = []
            def cargo(inner, name, *args):
                inner.calls.append((name, args))
                if name == 'catalog-fixture-artifacts':
                    graph = self.catalog_graph()
                elif name.startswith('client-'):
                    graph = self.client_graph(name == 'client-fixture-artifacts')
                elif name == 'helper-release-artifacts':
                    graph = self.helper_graph()
                elif name == 'helper-fixture-artifacts':
                    graph = self.helper_graph(True)
                else:
                    graph = [{'reason': 'compiler-artifact', 'target': {'name': 'theseus'}, 'features': []},
                             {'reason': 'compiler-artifact', 'target': {'name': 'theseus_host'},
                              'features': ['test-support'] if name == 'app-test-artifacts' else []}]
                return '\n'.join(map(json.dumps, graph))
        runner = GraphRunner()
        validation.feature_boundaries(runner)
        calls = dict(runner.calls)
        self.assertEqual(calls.get('helper-release-artifacts'),
                         ('check', '--locked', '--release', '-p', 'theseus-native-helper', '--bin',
                          'theseus-native-helper', '--message-format=json'))
        self.assertEqual(calls.get('helper-fixture-artifacts'),
                         ('check', '--locked', '-p', 'theseus-native-helper', '--features', 'test-support',
                          '--bin', 'theseus-native-helper-fixture', '--message-format=json'))

    def client_graph(self, fixture=False):
        return [{'reason': 'compiler-artifact', 'target': {'name': name, 'kind': [kind]},
                 'features': ['test-support'] if fixture and name != 'theseus_host' else []}
                for name, kind in [('theseus_host', 'lib'), ('theseus_native_client', 'lib')] +
                ([('fixture_client', 'test')] if fixture else [])]

    def test_catalog_builder_parser_refuses_malformed_json_and_ambiguous_executable(self):
        builder_spec = importlib.util.spec_from_file_location('fixture_builder', SCRIPT.parents[1] / 'tests/support/build_host_fixture.py')
        builder = importlib.util.module_from_spec(builder_spec)
        builder_spec.loader.exec_module(builder)
        class Result:
            stdout = 'not Cargo JSON\n'
            def check_returncode(inner):
                pass
        with patch.object(builder.subprocess, 'run', return_value=Result()):
            with self.assertRaises(json.JSONDecodeError):
                builder.build(['--features', 'native-catalog-test-support'])
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            source = root / 'reader'
            source.write_bytes(b'inert reader')
            artifact = {'target': {'name': 'theseus-native-base-reader'}, 'executable': str(source)}
            with self.assertRaises(RuntimeError):
                builder.pin(root, [], 'theseus-native-base-reader', 'missing')
            with self.assertRaises(RuntimeError):
                builder.pin(root, [artifact, dict(artifact)], 'theseus-native-base-reader', 'ambiguous')
            self.assertFalse((root / 'ambiguous').exists())

    def test_catalog_graph_rejects_missing_duplicate_malformed_and_fault_features(self):
        graph = self.catalog_graph()
        validation.check_catalog_graph(graph)
        for index in range(len(graph)):
            with self.subTest(missing=index), self.assertRaises(validation.GateFailure):
                validation.check_catalog_graph(graph[:index] + graph[index + 1:])
        for target in ('theseus_host', 'theseus_native_client', 'theseus'):
            bad = json.loads(json.dumps(graph))
            next(item for item in bad if item['target']['name'] == target)['features'].append('test-support')
            with self.subTest(fault=target), self.assertRaises(validation.GateFailure):
                validation.check_catalog_graph(bad)
        helper_fault = {'reason': 'compiler-artifact', 'target': {'name': 'theseus_native_helper', 'kind': ['lib']}, 'features': ['test-support']}
        for malformed in (None, {}, [None], graph + [graph[-1]], graph + [helper_fault]):
            with self.subTest(malformed=malformed), self.assertRaises(validation.GateFailure):
                validation.check_catalog_graph(malformed)
        bad = json.loads(json.dumps(graph))
        bad[-1]['features'] = []
        with self.assertRaises(validation.GateFailure):
            validation.check_catalog_graph(bad)

    def test_catalog_metadata_refuses_missing_tampered_substituted_and_aliased_reader(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            metadata = {}
            for key in ('fixture', 'application', 'production', 'mcp_fixture', 'helper_production', 'helper_fixture'):
                path = root / key
                path.write_bytes(key.encode())
                path.chmod(0o755)
                metadata[key] = {'path': str(path), 'sha256': validation.sha256(path)}
            self.add_catalog_metadata(root, metadata)
            validation.verify_artifacts(metadata)
            for key in validation.CATALOG_ARTIFACTS:
                bad = json.loads(json.dumps(metadata))
                del bad[key]
                with self.subTest(missing=key), self.assertRaises(validation.GateFailure):
                    validation.verify_artifacts(bad)
            for malformed in (None, [], {'native_base_reader': []}):
                with self.subTest(metadata=malformed), self.assertRaises(validation.GateFailure):
                    validation.verify_artifacts(malformed)
            for field, value in (('path', None), ('sha256', 'not-a-digest')):
                bad = json.loads(json.dumps(metadata))
                bad['native_base_reader'][field] = value
                with self.subTest(field=field), self.assertRaises(validation.GateFailure):
                    validation.verify_artifacts(bad)
            bad = json.loads(json.dumps(metadata))
            bad['native_base_reader']['cargo_artifact'] = bad['native_target_reader']['cargo_artifact']
            with self.assertRaises(validation.GateFailure):
                validation.verify_artifacts(bad)
            bad = json.loads(json.dumps(metadata))
            bad['native_base_reader'].update(path=bad['native_target_reader']['path'], sha256=bad['native_target_reader']['sha256'])
            with self.assertRaises(validation.GateFailure):
                validation.verify_artifacts(bad)
            bad = json.loads(json.dumps(metadata))
            del bad['native_catalog_graph']
            with self.assertRaises(validation.GateFailure):
                validation.verify_artifacts(bad)
            pathlib.Path(metadata['native_target_reader']['path']).write_bytes(b'replaced reader')
            with self.assertRaises(validation.GateFailure):
                validation.verify_artifacts(metadata)

    def test_catalog_runtime_inventory_and_commands_are_required_on_every_native_os(self):
        class RecordingRunner(StageRunner):
            def cargo(inner, name, *args, **kwargs):
                inner.calls.append((name, args, kwargs))
            def run(inner, *args, **kwargs):
                pass
        for system in ('Linux', 'Darwin', 'Windows'):
            with self.subTest(system=system), tempfile.TemporaryDirectory() as temp:
                runner = RecordingRunner(pathlib.Path(temp), {})
                with patch.object(validation, 'load_pinned', return_value={}), patch.object(validation.platform, 'system', return_value=system):
                    validation.native(runner, 'precompile')
                    validation.native(runner, 'runtime')
                calls = {name: (args, kwargs) for name, args, kwargs in runner.calls}
                for stage in ('native-profile-precompile', 'native-catalog-precompile', 'native-consumers-precompile'):
                    self.assertIn('--no-run', calls[stage][0])
                for stage, target, cases in (
                        ('native-profile-runtime', 'native_profile_runtime', 'profile-runtime-common'),
                        ('native-catalog-adapter', 'native_catalog_adapter', 'catalog-common'),
                        ('native-selected', 'native_selected', 'selected-common'),
                        ('native-recovery', 'native_recovery', 'recovery-common'),
                        ('native-consumers', 'native_consumers', 'consumers-common')):
                    args, kwargs = calls[stage]
                    self.assertEqual(args[args.index('--test') + 1], target)
                    self.assertTrue(validation.CASE_INVENTORY['native'][cases])
                    self.assertTrue(set(validation.CASE_INVENTORY['native'][cases]).issubset(kwargs['required_cases']))
                    self.assertIn('--test-threads=1', args)
                self.assertIn('fake_ssh_argv_preparation_uses_only_synthetic_private_material', calls['native-catalog-adapter'][1]['required_cases'])
                args, kwargs = calls['native-catalog-library']
                self.assertIn('--lib', args)
                self.assertEqual(args[args.index('--features') + 1], 'native-catalog-test-support')
                self.assertEqual(kwargs['required_cases'], validation.CASE_INVENTORY['native']['catalog-library-common'])
                self.assertEqual(len(kwargs['required_cases']), 5)
                self.assertEqual('unsafe_reserved_name_is_not_absent_family' in calls['native-consumers'][1]['required_cases'], system != 'Windows')

    def test_client_graph_rejects_missing_features_host_faults_and_release_fixture(self):
        for fixture in (False, True):
            good = self.client_graph(fixture)
            validation.check_client_graph(good, fixture=fixture)
            for index in range(len(good)):
                with self.subTest(fixture=fixture, missing=index):
                    with self.assertRaises(validation.GateFailure):
                        validation.check_client_graph(good[:index] + good[index + 1:], fixture=fixture)
                bad = json.loads(json.dumps(good))
                bad[index]['features'] = [] if bad[index]['features'] else ['test-support']
                with self.subTest(fixture=fixture, wrong_features=index):
                    with self.assertRaises(validation.GateFailure):
                        validation.check_client_graph(bad, fixture=fixture)
                bad = json.loads(json.dumps(good))
                bad[index]['target']['kind'] = ['bin']
                with self.subTest(fixture=fixture, wrong_target_kind=index):
                    with self.assertRaises(validation.GateFailure):
                        validation.check_client_graph(bad, fixture=fixture)
        with self.assertRaises(validation.GateFailure):
            validation.check_client_graph(self.client_graph() + self.client_graph(True)[-1:])

    def test_client_library_and_explicit_fixture_graph_checks_are_required(self):
        calls = {}
        corrupted = {}
        def cargo(name, *args):
            calls[name] = args
            if name == 'catalog-fixture-artifacts':
                graph = self.catalog_graph()
            elif name.startswith('client-'):
                graph = corrupted.get(name, self.client_graph(name == 'client-fixture-artifacts'))
            elif name.startswith('helper-'):
                graph = self.helper_graph(name == 'helper-fixture-artifacts')
            else:
                graph = [{'reason': 'compiler-artifact', 'target': {'name': 'theseus'}, 'features': []},
                         {'reason': 'compiler-artifact', 'target': {'name': 'theseus_host'},
                          'features': ['test-support'] if name == 'app-test-artifacts' else []}]
            return '\n'.join(map(json.dumps, graph))
        with tempfile.TemporaryDirectory() as temp:
            runner = validation.Runner(pathlib.Path(temp))
            with patch.object(runner, 'cargo', side_effect=cargo):
                validation.feature_boundaries(runner)
            for name, graph in (
                ('client-release-artifacts', self.client_graph()[0:1]),
                ('client-release-artifacts', self.client_graph(True)),
                ('client-fixture-artifacts', self.client_graph(True)[:-1]),
                ('client-fixture-artifacts', [dict(self.client_graph(True)[0], features=['test-support'])] + self.client_graph(True)[1:]),
            ):
                corrupted.clear()
                corrupted[name] = graph
                with self.subTest(corrupted_graph=name, graph=graph), \
                        patch.object(runner, 'cargo', side_effect=cargo), \
                        self.assertRaises(validation.GateFailure):
                    validation.feature_boundaries(runner)
        self.assertEqual(calls.get('client-release-artifacts'),
                         ('check', '--locked', '--release', '-p', 'theseus-native-client', '--lib', '--message-format=json'))
        self.assertEqual(calls.get('client-fixture-artifacts'),
                         ('test', '--locked', '-p', 'theseus-native-client', '--features', 'test-support',
                          '--test', 'fixture_client', '--no-run', '--message-format=json'))

    def test_native_client_gate_requires_actual_os_inventory_and_exact_target(self):
        class RecordingRunner(StageRunner):
            def cargo(inner, name, *args, **kwargs):
                inner.calls.append((name, args, kwargs))
            def run(inner, *args, **kwargs):
                pass
        common = ['native_client_preflight_is_exact_and_read_only',
                  'native_client_requires_committed_intent_and_recovers_same_lease',
                  'native_client_run_and_query_preserve_native_outcome',
                  'native_client_cancel_remains_responsive_under_output_pressure',
                  'native_client_connection_loss_never_reexecutes_run',
                  'native_client_reconnect_ids_and_partial_failure_owners_are_safe',
                  'native_client_transport_join_does_not_claim_remote_cleanup',
                  'native_client_file_roundtrip_empty_bounded_ranges_and_run',
                  'native_client_file_connection_loss_queries_original_without_second_create']
        for system in ('Linux', 'Darwin', 'Windows'):
            with self.subTest(system=system), tempfile.TemporaryDirectory() as temp:
                runner = RecordingRunner(pathlib.Path(temp), {})
                with patch.object(validation, 'load_pinned', return_value={}), \
                        patch.object(validation.platform, 'system', return_value=system):
                    validation.native(runner, 'precompile')
                    validation.native(runner, 'runtime')
                calls = {name: (args, kwargs) for name, args, kwargs in runner.calls}
                prefix = ('test', '--locked', '-p', 'theseus-native-client', '--features', 'test-support', '--test', 'fixture_client')
                self.assertEqual(calls.get('native-client-precompile'), (prefix + ('--no-run',), {'timeout': 240}))
                expected = common + (['native_client_windows_unsigned_status_is_preserved'] if system == 'Windows' else [])
                self.assertEqual(calls.get('native-client-runtime'), (prefix + ('--', '--nocapture', '--test-threads=1'),
                                                                    {'timeout': 240, 'required_cases': expected}))
                receipt = self.cargo_transcript(expected, expected)
                self.assertTrue(receipt['test_execution_verified'])
                self.assert_required_cases_rejected(expected[:-1], expected)

    def test_ignored_duplicate_and_nested_noisy_receipts_cannot_satisfy_required_cases(self):
        for text in (
            'test required ... ignored\ntest result: ok. 0 passed; 0 failed; 1 ignored;',
            'test required ... ok\ntest required ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored;',
            'test required ... child log\ntest nested ... ok\nok\ntest result: ok. 1 passed; 0 failed; 0 ignored;',
        ):
            self.assertFalse(validation.test_execution(text, ['required'])['test_execution_verified'])

    def test_source_hash_changes_for_uncommitted_content_and_names(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            (root / 'a').write_bytes(b'one')
            before = validation.tree_digest(root, [b'a'])
            (root / 'a').write_bytes(b'two')
            self.assertNotEqual(before, validation.tree_digest(root, [b'a']))
            (root / 'a').rename(root / 'b')
            self.assertNotEqual(before, validation.tree_digest(root, [b'b']))

    def test_native_identity_rejects_cross_compile_claim(self):
        with self.assertRaises(validation.GateFailure):
            validation.check_native_host('host: x86_64-unknown-linux-gnu', 'Windows', 'x86_64-pc-windows-msvc')
        with self.assertRaises(validation.GateFailure):
            validation.check_native_host('host: x86_64-pc-windows-msvc', 'Linux', 'x86_64-pc-windows-msvc')
        self.assertEqual(validation.check_native_host('host: aarch64-apple-darwin', 'Darwin', None), 'aarch64-apple-darwin')

    def test_budget_exhaustion_marks_suite_failure_without_running(self):
        with tempfile.TemporaryDirectory() as temp:
            runner = validation.Runner(pathlib.Path(temp), pathlib.Path.cwd(), deadline=0)
            with self.assertRaises(validation.GateFailure):
                runner.run('docker-suite', [sys.executable, '-c', 'raise SystemExit("must not run")'])
            receipt = json.loads((pathlib.Path(temp) / 'docker-suite.result.json').read_text())
            self.assertEqual(receipt['status'], 'not_run_budget_exhausted')
            self.assertFalse(receipt['pass'])


    def test_integration_test_graph_rejects_missing_swapped_or_fault_artifacts(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            graph = []
            metadata = {'native_integration_test_graph': graph}
            for target in validation.INTEGRATION_TEST_ARTIFACTS:
                path = root / target
                path.write_bytes(target.encode())
                path.chmod(0o755)
                artifact = {'reason': 'compiler-artifact', 'target': {'name': target, 'kind': ['test']},
                            'features': ['native-integration-test-support'], 'executable': str(path)}
                graph.append(artifact)
                metadata[target] = {'path': str(path), 'sha256': validation.sha256(path), 'cargo_artifact': artifact}
            # Root integration tests intentionally have the existing Host dev
            # dependency; separately pinned executed binaries remain fault-free.
            graph.append({'reason': 'compiler-artifact', 'target': {'name': 'theseus_host', 'kind': ['lib']}, 'features': ['test-support']})
            validation.verify_integration_tests(metadata)
            for mutation in ('missing', 'swapped', 'fault', 'tampered'):
                bad = copy.deepcopy(metadata)
                if mutation == 'missing':
                    del bad['native_desktop']
                elif mutation == 'swapped':
                    bad['native_desktop']['cargo_artifact'] = bad['native_services']['cargo_artifact']
                elif mutation == 'fault':
                    bad['native_integration_test_graph'].append({'reason': 'compiler-artifact',
                        'target': {'name': 'theseus_native_helper', 'kind': ['lib']}, 'features': ['test-support']})
                else:
                    bad['native_desktop']['sha256'] = '0' * 64
                with self.subTest(mutation=mutation), self.assertRaises(validation.GateFailure):
                    validation.verify_integration_tests(bad)


    def test_linux_inventory_requires_all_source_desktop_ordinary_cases_and_bound(self):
        identity = {'source_sha': 'a' * 40, 'dirty': False}
        metadata = dict(identity)
        for key in ('helper_production', 'helper_fixture', 'desktop_broker_fixture',
                    *validation.INTEGRATION_ARTIFACTS, *validation.INTEGRATION_TEST_ARTIFACTS):
            metadata[key] = {'path': '/private/pinned/' + key, 'sha256': 'b' * 64, 'length': 12}
        with tempfile.TemporaryDirectory() as temp:
            runner = validation.Runner(pathlib.Path(temp))
            runner.host = 'x86_64-unknown-linux-gnu'
            with patch.object(validation, 'load_pinned', return_value=metadata), patch.object(validation, 'verify_integration_tests'), patch.object(validation, 'source_identity', return_value=identity), patch.object(validation.platform, 'system', return_value='Linux'), patch.object(runner, 'run') as run:
                validation.native(runner, 'integration')
            desktop = next(call for call in run.call_args_list if call.args[0] == 'source-desktop-product')
            self.assertEqual(set(desktop.kwargs['required_cases']), {
                'source_desktop_clean_uses_original_broker_child_and_public_cli_saved_views',
                'source_desktop_missing_and_leaked_entries_fail_exact_sut_postconditions',
                'source_desktop_fresh_six_tool_mcp_matches_own_saved_desktop_and_tui',
                'source_desktop_upgrade_keeps_original_base_and_target_generations',
                'source_desktop_reinstall_same_tuple_requires_new_original_sut_generation',
                'source_desktop_ordinary_fresh_six_tool_mcp_keeps_exact_generations_and_saved_views',
            })
            self.assertGreaterEqual(desktop.kwargs['timeout'], 300)
            self.assertLessEqual(desktop.kwargs['timeout'], 900)

    def test_every_native_os_requires_source_desktop_product_cases_and_nonzero_execution(self):
        identity = {'source_sha': 'a' * 40, 'dirty': False}
        metadata = dict(identity)
        for key in ('helper_production', 'helper_fixture', 'desktop_broker_fixture',
                    *validation.INTEGRATION_ARTIFACTS, *validation.INTEGRATION_TEST_ARTIFACTS):
            metadata[key] = {'path': '/private/pinned/' + key, 'sha256': 'b' * 64, 'length': 12}
        for system, target in [('Linux', 'x86_64-unknown-linux-gnu'), ('Darwin', 'aarch64-apple-darwin'), ('Windows', 'x86_64-pc-windows-msvc')]:
            with self.subTest(system=system), tempfile.TemporaryDirectory() as temp:
                runner = validation.Runner(pathlib.Path(temp))
                runner.host = target
                with patch.object(validation, 'load_pinned', return_value=metadata), patch.object(validation, 'verify_integration_tests'), patch.object(validation, 'source_identity', return_value=identity), patch.object(validation.platform, 'system', return_value=system), patch.object(runner, 'run') as run:
                    validation.native(runner, 'integration')
                desktop = [call for call in run.call_args_list if call.args[0] == 'source-desktop-product']
                self.assertEqual(len(desktop), 1, 'every native OS must execute its actual owned product suite')
                call = desktop[0]
                self.assertTrue(call.kwargs['require_tests'])
                self.assertEqual(len(call.kwargs['required_cases']), 6)
                self.assertIn('source_desktop_clean_uses_original_broker_child_and_public_cli_saved_views', call.kwargs['required_cases'])
                self.assertIn('source_desktop_ordinary_fresh_six_tool_mcp_keeps_exact_generations_and_saved_views', call.kwargs['required_cases'])
                self.assertEqual(call.args[1][0], metadata['native_desktop']['path'])
                self.assertLessEqual(call.kwargs['timeout'], 900)

    def test_genuine_owned_entry_cases_are_required_only_on_mac_and_windows(self):
        identity = {'source_sha': 'a' * 40, 'dirty': False}
        metadata = dict(identity)
        for key in ('helper_production', 'helper_fixture', 'desktop_broker_fixture',
                    *validation.INTEGRATION_ARTIFACTS, *validation.INTEGRATION_TEST_ARTIFACTS):
            metadata[key] = {'path': '/private/pinned/' + key, 'sha256': 'b' * 64, 'length': 12}
        for system, target in [('Linux', 'x86_64-unknown-linux-gnu'), ('Darwin', 'aarch64-apple-darwin'), ('Windows', 'x86_64-pc-windows-msvc')]:
            with self.subTest(system=system), tempfile.TemporaryDirectory() as temp:
                runner = validation.Runner(pathlib.Path(temp))
                runner.host = target
                with patch.object(validation, 'load_pinned', return_value=metadata), patch.object(validation, 'verify_integration_tests'), patch.object(validation, 'source_identity', return_value=identity), patch.object(validation.platform, 'system', return_value=system), patch.object(runner, 'run') as run:
                    validation.native(runner, 'integration')
                desktop = [call for call in run.call_args_list if call.args[0] == 'source-owned-native-entry-product']
                if system == 'Linux':
                    self.assertEqual(desktop, [])
                    continue
                self.assertEqual(len(desktop), 1)
                call = desktop[0]
                self.assertTrue(call.kwargs['require_tests'])
                self.assertEqual(set(call.kwargs['required_cases']), {
                    'source_owned_native_entry_clean_releases_original_and_preserves_saved_views',
                    'source_owned_native_entry_upgrade_releases_each_original_generation',
                    'source_owned_native_entry_reinstall_releases_same_tuple_new_generation',
                })
                self.assertEqual(call.args[1][0], metadata['native_desktop']['path'])
                self.assertLessEqual(call.kwargs['timeout'], 900)

    def test_linux_manager_manifest_pins_real_combined_cases_and_source_owners(self):
        metadata = {'source_sha': 'a' * 40, 'dirty': False}
        for key in ('helper_production', 'helper_fixture', 'desktop_broker_fixture',
                    *validation.INTEGRATION_ARTIFACTS, *validation.INTEGRATION_TEST_ARTIFACTS):
            metadata[key] = {'path': '/private/pinned/' + key, 'sha256': 'b' * 64, 'length': 12}
        manifest = validation.manager_manifest(metadata, 'x86_64-unknown-linux-gnu', 'Linux')
        expected = {
            ('native_services', 'systemd_user_clean_installer_managed_service_uses_shared_product_path'),
            ('native_services', 'systemd_installer_missing_service_is_failed_without_harness_repair'),
            ('native_services', 'systemd_uninstaller_leaves_service_is_failed_before_and_after_provider_disposal'),
            ('native_services', 'systemd_preexisting_or_wrong_owned_unit_is_untouched'),
            ('native_service_mcp', 'systemd_supplied_cli_and_fresh_mcp_normalized_frontend_parity'),
            ('native_desktop', 'source_combined_service_desktop_share_one_sequence_provider_and_lease'),
            ('native_desktop', 'source_combined_ordinary_fresh_six_tool_mcp_keeps_one_shared_lease_and_saved_views'),
        }
        self.assertEqual({(c['binary'], c['name']) for c in manifest['cases']}, expected)
        for role, key in {'native_desktop': 'native_desktop', 'THESEUS_NATIVE_DESKTOP_HELPER_BINARY': 'helper_fixture', 'THESEUS_NATIVE_DESKTOP_BROKER_BINARY': 'desktop_broker_fixture', 'THESEUS_NATIVE_HELPER_BINARY': 'helper_production'}.items():
            self.assertEqual(manifest['artifacts'][role]['path'], metadata[key]['path'])
        for key in ['native_desktop', 'helper_fixture', 'desktop_broker_fixture']:
            bad = copy.deepcopy(metadata)
            bad.pop(key)
            with self.subTest(missing=key), self.assertRaises(validation.GateFailure):
                validation.manager_manifest(bad, 'x86_64-unknown-linux-gnu', 'Linux')

    def test_manager_manifest_has_exact_native_roles_and_rejects_dirty_source(self):
        metadata = {'source_sha': 'a' * 40, 'dirty': False}
        for key in ('helper_production', 'helper_fixture', 'desktop_broker_fixture',
                    *validation.INTEGRATION_ARTIFACTS, *validation.INTEGRATION_TEST_ARTIFACTS):
            metadata[key] = {'path': '/private/pinned/' + key, 'sha256': 'b' * 64, 'length': 12}
        for system, target, test in [('Linux', 'x86_64-unknown-linux-gnu', 'native_services'),
                                     ('Darwin', 'aarch64-apple-darwin', 'native_services_platforms'),
                                     ('Windows', 'x86_64-pc-windows-msvc', 'native_services_platforms')]:
            with self.subTest(system=system):
                manifest = validation.manager_manifest(metadata, target, system)
                self.assertEqual(manifest['source_sha'], metadata['source_sha'])
                self.assertEqual(manifest['build']['target'], target)
                self.assertIn(test, manifest['artifacts'])
                self.assertEqual(len(manifest['cases']), 7)
                self.assertEqual(len(manifest['artifacts']), 10 if system == 'Linux' else 6)
                self.assertEqual('native_desktop' in manifest['artifacts'], system == 'Linux')
                self.assertFalse(any('model' in c['name'] for c in manifest['cases']))
                self.assertEqual(manifest['artifacts']['THESEUS_NATIVE_HELPER_BINARY']['path'], metadata['helper_production']['path'])
                if system == 'Linux':
                    import prepare_native_systemd_ci as operator
                elif system == 'Darwin':
                    import prepare_native_launchd_ci as operator
                else:
                    import prepare_native_scm_ci as operator
                    for role, pin in manifest['artifacts'].items():
                        pin['path'] = 'C:\\pinned\\' + role
                plan = operator.make_plan(manifest, 'abcd1234abcd1234')
                self.assertEqual(plan['token'], 'abcd1234abcd1234')

        with self.assertRaises(validation.GateFailure):
            validation.manager_manifest(dict(metadata, dirty=True), 'x86_64-unknown-linux-gnu', 'Linux')
        with self.assertRaises(validation.GateFailure):
            validation.manager_manifest(metadata, 'x86_64-pc-windows-msvc', 'Linux')


    def test_integration_stage_requires_registered_cases_and_refuses_source_drift(self):
        identity = {'source_sha': 'a' * 40, 'dirty': False}
        metadata = dict(identity)
        for key in ('helper_production', 'helper_fixture', 'desktop_broker_fixture',
                    *validation.INTEGRATION_ARTIFACTS, *validation.INTEGRATION_TEST_ARTIFACTS):
            metadata[key] = {'path': '/private/pinned/' + key, 'sha256': 'b' * 64, 'length': 12}
        for system, host in [('Linux', 'x86_64-unknown-linux-gnu'), ('Windows', 'x86_64-pc-windows-msvc'), ('Darwin', 'aarch64-apple-darwin')]:
            with self.subTest(system=system), tempfile.TemporaryDirectory() as temp:
                runner = validation.Runner(pathlib.Path(temp))
                runner.host = host
                with patch.object(validation, 'load_pinned', return_value=metadata), patch.object(validation, 'verify_integration_tests') as verify, patch.object(validation, 'source_identity', return_value=identity), patch.object(validation.platform, 'system', return_value=system), patch.object(runner, 'run') as run:
                    validation.native(runner, 'integration')
                verify.assert_called_once_with(metadata)
                self.assertEqual(len(run.call_args_list), 4 if system == 'Linux' else 5)
                self.assertTrue(all(call.kwargs.get('require_tests') and call.kwargs.get('required_cases') for call in run.call_args_list))
                manifest = json.loads((runner.evidence / 'native-manager-manifest.json').read_text())
                self.assertEqual(manifest['build']['target'], host)
                with patch.object(validation, 'load_pinned', return_value=metadata), patch.object(validation, 'verify_integration_tests'), patch.object(validation, 'source_identity', return_value=dict(identity, source_sha='c' * 40)), patch.object(runner, 'run') as run, self.assertRaises(validation.GateFailure):
                    validation.native(runner, 'integration')
                run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
