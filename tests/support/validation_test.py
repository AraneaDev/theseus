"""Behavioral checks for the shared local/CI validation driver (no Cargo/Docker)."""
import importlib.util
import json
import pathlib
import sys
import tempfile
import unittest
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
                                     ['workspace-tests', 'native-cli-terminal', 'native-explicit-terminal-controls'])
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
            metadata = {'fixture': {'path': str(artifact), 'sha256': validation.sha256(artifact)},
                        'application': {'path': str(artifact), 'sha256': validation.sha256(artifact)}}
            metadata['production'] = dict(metadata['application'])
            metadata['mcp_fixture'] = dict(metadata['fixture'])
            validation.verify_artifacts(metadata)
            artifact.write_bytes(b'replaced')
            with self.assertRaises(validation.GateFailure):
                validation.verify_artifacts(metadata)
            artifact.unlink()
            with self.assertRaises(validation.GateFailure):
                validation.verify_artifacts(metadata)

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
