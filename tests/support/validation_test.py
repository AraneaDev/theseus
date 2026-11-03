"""Behavioral checks for the shared local/CI validation driver (no Cargo/Docker)."""
import importlib.util
import io
import json
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
                                     ['workspace-tests', 'native-cli-terminal', 'native-explicit-terminal-controls', 'native-helper-runtime'])
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
            validation.verify_artifacts(metadata)
            artifact.write_bytes(b'replaced')
            with self.assertRaises(validation.GateFailure):
                validation.verify_artifacts(metadata)
            artifact.unlink()
            with self.assertRaises(validation.GateFailure):
                validation.verify_artifacts(metadata)



    def test_nonexecutable_pinned_helper_is_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            metadata = {}
            for key in ('fixture', 'application', 'production', 'mcp_fixture', 'helper_production', 'helper_fixture'):
                path = root / key
                path.write_bytes(key.encode())
                path.chmod(0o755)
                metadata[key] = {'path': str(path), 'sha256': validation.sha256(path)}
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
            def graph(names, fixture=False, host_support=False):
                artifacts = []
                for name in names:
                    path = cargo / name
                    path.write_bytes((name + ('-fixture' if fixture else '-default')).encode())
                    path.chmod(0o755)
                    artifacts.append({'reason': 'compiler-artifact', 'target': {'name': name},
                                      'features': ['test-support'] if (host_support and name == 'theseus_host') or
                                      (fixture and name in ('theseus_native_helper', 'theseus-native-helper-fixture')) else [],
                                      'executable': str(path) if '-' in name or name == 'theseus' else None})
                return artifacts
            def fake_build(args):
                if args == ['--bin', 'theseus']:
                    return graph(('theseus_host', 'theseus'))
                if args == ['-p', 'theseus-native-helper', '--bin', 'theseus-native-helper']:
                    return graph(('theseus_host', 'theseus_native_helper', 'theseus-native-helper'))
                if args == ['--workspace', '--bins', '--features', 'theseus-host/test-support']:
                    (cargo / 'theseus-native-helper').write_bytes(b'overwritten-feature-unified-helper')
                    return graph(('theseus_host', 'theseus-host-fixture', 'theseus'), host_support=True)
                if args == ['--features', 'mcp-test-support', '--bin', 'theseus-mcp-fixture']:
                    return graph(('theseus-mcp-fixture',))
                if args == ['-p', 'theseus-native-helper', '--features', 'test-support', '--bin', 'theseus-native-helper-fixture']:
                    return graph(('theseus_host', 'theseus_native_helper', 'theseus-native-helper-fixture'), fixture=True)
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
                                  ('helper_fixture', 'THESEUS_NATIVE_HELPER_FIXTURE')):
                self.assertEqual(runner.env[variable], metadata[key]['path'])
                self.assertIn(f'{variable}={metadata[key]["path"]}\n', env_file.read_text())
            self.assertEqual(json.loads((runner.evidence / 'native-artifacts.json').read_text()), metadata)

    def test_native_helper_gate_requires_actual_host_inventory_and_exact_target(self):
        class RecordingRunner(StageRunner):
            def cargo(inner, name, *args, **kwargs):
                inner.calls.append((name, args, kwargs))
            def run(inner, *args, **kwargs):
                pass
        common = ['explicit_config_and_unsupported_requests_never_create_authority',
                  'ordinary_pipes_prove_commit_before_ack_spawn_and_finished',
                  'lost_acquire_reply_reuses_committed_intent_and_lease',
                  'cached_cancel_remains_responsive_under_saturated_output',
                  'finished_is_durable_before_optional_output_drain',
                  'native_child_status_and_probe_principal_are_observed_truthfully',
                  'eof_and_unresolved_cleanup_keep_owners_until_join_or_exit_two',
                  'fixture_faults_are_absent_from_normal_helper_graph']
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
                if name == 'helper-release-artifacts':
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


if __name__ == '__main__':
    unittest.main()
