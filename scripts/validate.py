#!/usr/bin/env python3
"""Required native/Docker validation shared by local pre-push use and CI.

No runner registration, credential discovery, remote execution or Git hook changes.
Receipts identify the current source bytes, including uncommitted source files.
"""
import argparse
import hashlib
import json
import os
import pathlib
import platform
import re
import signal
import stat
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
DOCKER_SUITES = ('docker_lifecycle', 'interruption_lifecycle', 'downgrade_lifecycle',
                 'rollback_lifecycle', 'interrupted_rollback_lifecycle',
                 'comparison_docker', 'mcp_docker', 'mcp_server_docker')
CASE_INVENTORY = json.loads((ROOT / 'scripts/validation_cases.json').read_text(encoding='utf-8'))
NATIVE_STAGES = ('fixture', 'stdio', 'fmt', 'clippy', 'portable', 'features',
                 'precompile', 'early-controls', 'early-cli', 'runtime', 'integration', 'release')


class GateFailure(RuntimeError):
    pass


def console(text, *, file=None, end='\n', flush=False):
    """Escape unrepresentable display characters without changing raw evidence."""
    stream = sys.stdout if file is None else file
    encoding = getattr(stream, 'encoding', None)
    if encoding:
        text = text.encode(encoding, errors='backslashreplace').decode(encoding)
    print(text, file=stream, end=end, flush=flush)


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n', encoding='utf-8')


def sha256(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def tree_digest(root, names):
    """Hash paths, types, executable bits and bytes; no mtime-based shortcuts."""
    digest = hashlib.sha256()
    for name in sorted(set(names)):
        path = root / os.fsdecode(name)
        digest.update(len(name).to_bytes(8, 'big') + name)
        try:
            info = path.lstat()
        except FileNotFoundError:
            digest.update(b'deleted')
            continue
        digest.update((info.st_mode & (stat.S_IFMT(info.st_mode) | 0o111)).to_bytes(8, 'big'))
        if path.is_symlink():
            content = os.fsencode(os.readlink(path))
        elif path.is_file():
            content = bytes.fromhex(sha256(path))
        else:
            raise GateFailure(f'Unsupported source entry: {path}')
        digest.update(len(content).to_bytes(8, 'big') + content)
    return digest.hexdigest()


def source_identity(root):
    def git(*args):
        return subprocess.check_output(['git', *args], cwd=root)
    names = git('ls-files', '--cached', '--others', '--exclude-standard', '-z').split(b'\0')
    return {'source_sha': git('rev-parse', 'HEAD').decode('ascii').strip(),
            'dirty': bool(git('status', '--porcelain', '--untracked-files=all')),
            'source_tree_sha256': tree_digest(root, [name for name in names if name]),
            'git_diff_sha256': hashlib.sha256(git('diff', '--binary', 'HEAD')).hexdigest()}


def check_native_host(rustc, system, expected):
    hosts = [line.removeprefix('host: ') for line in rustc.splitlines() if line.startswith('host: ')]
    suffix = {'Linux': '-unknown-linux-gnu', 'Darwin': '-apple-darwin', 'Windows': '-pc-windows-msvc'}.get(system)
    if len(hosts) != 1 or not suffix or not hosts[0].endswith(suffix) or (expected and hosts[0] != expected):
        raise GateFailure(f'Native compiler host mismatch: OS={system}, expected={expected}, hosts={hosts}')
    return hosts[0]


CATALOG_ARTIFACTS = {
    'native_catalog_fixture': ('theseus-native-catalog-fixture', 'THESEUS_NATIVE_CATALOG_FIXTURE'),
    'native_base_reader': ('theseus-native-base-reader', 'THESEUS_NATIVE_BASE_READER'),
    'native_target_reader': ('theseus-native-target-reader', 'THESEUS_NATIVE_TARGET_READER'),
}


INTEGRATION_ARTIFACTS = {
    'native_service_fixture': ('theseus-native-service-fixture', 'THESEUS_NATIVE_SERVICE_FIXTURE'),
    'native_service_base': ('theseus-native-service-base', 'THESEUS_NATIVE_SERVICE_BASE'),
    'native_service_target': ('theseus-native-service-target', 'THESEUS_NATIVE_SERVICE_TARGET'),
    'native_integration_controller': ('theseus-native-integration-controller', 'THESEUS_NATIVE_INTEGRATION_CONTROLLER'),
}


INTEGRATION_TEST_ARTIFACTS = ('native_services', 'native_service_mcp', 'native_services_platforms', 'native_desktop')


def verify_integration_tests(metadata):
    graph = metadata.get('native_integration_test_graph')
    if not isinstance(graph, list) or any(not isinstance(item, dict) for item in graph):
        raise GateFailure('Missing independent integration test graph')
    paths = set()
    for target in INTEGRATION_TEST_ARTIFACTS:
        pin = metadata.get(target)
        if not isinstance(pin, dict):
            raise GateFailure(f'{target}: missing pinned test')
        artifact = pin.get('cargo_artifact')
        matches = [item for item in graph if item.get('target', {}).get('name') == target
                   and item.get('target', {}).get('kind') == ['test'] and item.get('executable')]
        if len(matches) != 1 or artifact != matches[0] or 'native-integration-test-support' not in artifact.get('features', []):
            raise GateFailure(f'{target}: test graph substitution or feature mismatch')
        path = pathlib.Path(pin.get('path', ''))
        if not path.is_file() or path.is_symlink() or (os.name != 'nt' and not os.access(path, os.X_OK)) or sha256(path) != pin.get('sha256'):
            raise GateFailure(f'{target}: changed pinned integration test')
        canonical = os.path.normcase(str(path.resolve()))
        if canonical in paths:
            raise GateFailure('Integration tests must have distinct copied paths')
        paths.add(canonical)
    for item in graph:
        if item.get('target', {}).get('name') in {'theseus_native_helper', 'theseus_native_client'} and 'test-support' in item.get('features', []):
            raise GateFailure('Integration test graph contains helper/client fault support')


def manager_manifest(metadata, target, system):
    suffix = {'Linux': '-unknown-linux-gnu', 'Darwin': '-apple-darwin', 'Windows': '-pc-windows-msvc'}.get(system)
    if not suffix or not target.endswith(suffix) or metadata.get('dirty') is not False or not re.fullmatch(r'[a-f0-9]{40}|[a-f0-9]{64}', metadata.get('source_sha', '')):
        raise GateFailure('Manager CI requires clean exact source and native target')
    source = metadata['source_sha']
    roles = {'THESEUS_NATIVE_HELPER_BINARY': 'helper_production'}
    roles.update({variable: key for key, (_, variable) in INTEGRATION_ARTIFACTS.items()})
    if system == 'Linux':
        roles.update(native_services='native_services', native_service_mcp='native_service_mcp', native_desktop='native_desktop')
        roles.update(THESEUS_NATIVE_DESKTOP_HELPER_BINARY='helper_fixture',
                     THESEUS_NATIVE_DESKTOP_BROKER_BINARY='desktop_broker_fixture')
        cases = [('native_services', name) for name in (
            'systemd_user_clean_installer_managed_service_uses_shared_product_path',
            'systemd_installer_missing_service_is_failed_without_harness_repair',
            'systemd_uninstaller_leaves_service_is_failed_before_and_after_provider_disposal',
            'systemd_preexisting_or_wrong_owned_unit_is_untouched')]
        cases.append(('native_service_mcp', 'systemd_supplied_cli_and_fresh_mcp_normalized_frontend_parity'))
        cases += [('native_desktop', name) for name in (
            'source_combined_service_desktop_share_one_sequence_provider_and_lease',
            'source_combined_ordinary_fresh_six_tool_mcp_keeps_one_shared_lease_and_saved_views')]
    else:
        roles['native_services_platforms'] = 'native_services_platforms'
        names = [
            'native::native_upgrade_sut_transitions_from_running_and_preserves_data',
            'native::native_reinstall_compares_original_definition_reader_and_data',
            'native::native_installer_missing_and_leaked_registration_never_passes',
            'native::native_running_wrong_version_data_or_nonce_never_passes_readiness',
        ]
        names += ([
            'native::scm_own_process_clean_observes_config_status_readiness_and_delete_pending',
            'owners::scm_delete_success_remains_pending_until_original_handles_close_and_name_is_absent',
            'owners::scm_native_pipe_rejects_actual_helper_sid_before_challenge',
        ] if system == 'Windows' else [
            'native::launchd_user_clean_keeps_loaded_configuration_runtime_and_readiness_distinct',
            'owners::launchd_wrong_domain_and_replaced_plist_never_authorize_stop_or_bootout',
            'launchd_running_wrong_native_peer_never_passes_readiness',
        ])
        cases = [('native_services_platforms', name) for name in names]
    artifacts = {}
    for role, key in roles.items():
        pin = metadata.get(key)
        if not isinstance(pin, dict) or not isinstance(pin.get('path'), str) or not re.fullmatch(r'[a-f0-9]{64}', pin.get('sha256', '')):
            raise GateFailure(f'{role}: exact pinned manager artifact required')
        length = pin['length'] if 'length' in pin else pathlib.Path(pin['path']).stat().st_size
        if type(length) is not int or length <= 0:
            raise GateFailure(f'{role}: nonempty pinned manager artifact required')
        artifacts[role] = {'path': pin['path'], 'sha256': pin['sha256'], 'length': length, 'source_sha': source}
    return {'schema': 1, 'source_sha': source,
            'build': {'source_sha': source, 'target': target, 'profile': 'debug', 'features': ['native-integration-test-support']},
            'artifacts': artifacts, 'cases': [{'binary': binary, 'name': name} for binary, name in cases]}


def check_catalog_graph(artifacts):
    """Independent catalog graph must never unify host/helper/client fault support."""
    if not isinstance(artifacts, list) or any(
            not isinstance(item, dict) or item.get('reason') != 'compiler-artifact'
            or not isinstance(item.get('target'), dict)
            or not isinstance(item['target'].get('name'), str)
            or not isinstance(item['target'].get('kind'), list)
            or not isinstance(item.get('features'), list)
            or any(not isinstance(feature, str) for feature in item['features'])
            for item in artifacts):
        raise GateFailure('Malformed independent catalog Cargo graph')
    required = [('theseus_host', 'lib'), ('theseus_native_helper_store', 'lib'),
                ('theseus_native_client', 'lib'), ('theseus', 'lib')]
    required += [(target, 'bin') for target, _ in CATALOG_ARTIFACTS.values()]
    for target, kind in required:
        matches = [item for item in artifacts if item.get('reason') == 'compiler-artifact'
                   and item.get('target', {}).get('name') == target
                   and item.get('target', {}).get('kind') == [kind]]
        if len(matches) != 1:
            raise GateFailure(f'{target}: catalog artifact absent or malformed')
        for item in matches:
            features = item['features']
            if 'test-support' in features or 'mcp-test-support' in features:
                raise GateFailure(f'{target}: catalog graph contains fault support')
            if (target == 'theseus' or kind == 'bin') and 'native-catalog-test-support' not in features:
                raise GateFailure(f'{target}: catalog feature absent')
    for item in artifacts:
        if item.get('target', {}).get('name') in {'theseus_host', 'theseus_native_helper', 'theseus_native_client'} and 'test-support' in item.get('features', []):
            raise GateFailure('Catalog graph contains host/helper/client fault support')
    forbidden = {'theseus-host-fixture', 'theseus-native-helper-fixture', 'theseus-mcp-fixture'}
    if any(item.get('target', {}).get('name') in forbidden for item in artifacts):
        raise GateFailure('Catalog graph contains an unrelated fault fixture')


def verify_artifacts(metadata):
    if not isinstance(metadata, dict):
        raise GateFailure('Malformed pinned artifact metadata')
    for key in ('fixture', 'application', 'production', 'mcp_fixture', 'helper_production', 'helper_fixture', 'desktop_broker_production', 'desktop_broker_fixture', *CATALOG_ARTIFACTS, *INTEGRATION_ARTIFACTS):
        item = metadata.get(key, {})
        if not isinstance(item, dict) or not isinstance(item.get('path'), str) or not isinstance(item.get('sha256'), str) or not re.fullmatch(r'[0-9a-f]{64}', item['sha256']):
            raise GateFailure(f'Malformed pinned native artifact: {key}')
        path = pathlib.Path(item['path'])
        if not path.is_file() or path.is_symlink() or (os.name != 'nt' and not os.access(path, os.X_OK)) or sha256(path) != item.get('sha256'):
            raise GateFailure(f'Missing or changed pinned native artifact: {key}')

    graph = metadata.get('native_catalog_graph')
    if not isinstance(graph, list):
        raise GateFailure('Missing independent native catalog graph')
    check_catalog_graph(graph)
    catalog_paths = []
    for key, (target, _) in CATALOG_ARTIFACTS.items():
        artifact = metadata[key].get('cargo_artifact')
        if not isinstance(artifact, dict) or artifact not in graph or artifact.get('target', {}).get('name') != target or artifact.get('target', {}).get('kind') != ['bin']:
            raise GateFailure(f'{key}: pinned executable does not match its independent graph')
        path = pathlib.Path(metadata[key]['path']).resolve()
        if path in catalog_paths:
            raise GateFailure('Catalog fixture/readers must have distinct pinned paths')
        catalog_paths.append(path)

    graph = metadata.get('native_integration_graph')
    if not isinstance(graph, list) or any(
            not isinstance(item, dict) or item.get('reason') != 'compiler-artifact'
            or not isinstance(item.get('target'), dict)
            or not isinstance(item['target'].get('name'), str)
            or not isinstance(item['target'].get('kind'), list)
            or not isinstance(item.get('features'), list)
            or any(not isinstance(feature, str) for feature in item['features'])
            for item in graph):
        raise GateFailure('Missing or malformed independent native integration graph')
    integration_paths = []
    for key, (target, _) in INTEGRATION_ARTIFACTS.items():
        artifact = metadata[key].get('cargo_artifact')
        matches = [item for item in graph if isinstance(item, dict)
                   and item.get('target', {}).get('name') == target
                   and item.get('target', {}).get('kind') == ['bin']]
        if len(matches) != 1 or artifact != matches[0] or 'native-integration-test-support' not in artifact.get('features', []):
            raise GateFailure(f'{key}: pinned executable does not match its integration graph')
        path = pathlib.Path(metadata[key]['path']).resolve()
        if path in integration_paths:
            raise GateFailure('Integration fixture/readers/controller must have distinct pinned paths')
        integration_paths.append(path)
    for item in graph:
        if item.get('target', {}).get('name') in {'theseus_host', 'theseus_native_helper', 'theseus_native_client'} and 'test-support' in item.get('features', []):
            raise GateFailure('Integration fixture graph contains native fault support')

    for key, fixture_graph in (('desktop_broker_production', False), ('desktop_broker_fixture', True)):
        graph = metadata.get(key + '_graph')
        artifact = metadata[key].get('cargo_artifact')
        if not isinstance(graph, list) or not isinstance(artifact, dict) or artifact not in graph:
            raise GateFailure(f'{key}: missing exact broker graph/artifact')
        target = artifact.get('target')
        features = artifact.get('features')
        if not isinstance(target, dict) or target.get('name') != 'theseus-native-desktop-broker' or target.get('kind') != ['bin'] or not isinstance(features, list):
            raise GateFailure(f'{key}: malformed broker artifact')
        if ('test-support' in features) != fixture_graph:
            raise GateFailure(f'{key}: broker feature boundary violated')
        for name, feature in (('theseus_host', 'desktop-test-support'), ('theseus_native_protocol', 'test-support')):
            matches = [item for item in graph if isinstance(item, dict)
                       and item.get('target', {}).get('name') == name]
            if len(matches) != 1 or (feature in matches[0].get('features', [])) != fixture_graph:
                raise GateFailure(f'{key}: {name} source feature boundary violated')
    if pathlib.Path(metadata['desktop_broker_production']['path']).samefile(metadata['desktop_broker_fixture']['path']):
        raise GateFailure('Production and source fixture broker must be separately pinned artifacts')

    if pathlib.Path(metadata['helper_production']['path']).samefile(metadata['helper_fixture']['path']):
        raise GateFailure('Normal helper and helper fixture must be separate pinned artifacts')


def check_helper_graph(artifacts, *, fixture=False):
    """Require the explicit helper graph; helper test-support never enables host faults."""
    binary = 'theseus-native-helper-fixture' if fixture else 'theseus-native-helper'
    for target in ('theseus_host', 'theseus_native_helper', binary):
        matches = [item for item in artifacts if item.get('reason') == 'compiler-artifact'
                   and item.get('target', {}).get('name') == target]
        enabled = fixture and target != 'theseus_host'
        if not matches or any(('test-support' in item['features']) != enabled for item in matches):
            raise GateFailure(f'{target}: helper feature boundary violated or artifact absent')
    if not fixture and any(item.get('target', {}).get('name') == 'theseus-native-helper-fixture'
                           for item in artifacts):
        raise GateFailure('Normal helper graph contains the fixture binary')


def check_client_graph(artifacts, *, fixture=False):
    """The client fixture enables only client support, never host fault injection."""
    targets = [('theseus_host', 'lib'), ('theseus_native_client', 'lib')]
    if fixture:
        targets.append(('fixture_client', 'test'))
    for target, kind in targets:
        matches = [item for item in artifacts if item.get('reason') == 'compiler-artifact'
                   and item.get('target', {}).get('name') == target]
        enabled = fixture and target != 'theseus_host'
        if not matches or any(('test-support' in item['features']) != enabled
                              or item['target'].get('kind') != [kind] for item in matches):
            raise GateFailure(f'{target}: client feature boundary violated or artifact absent')
    if not fixture and any(item.get('target', {}).get('name') == 'fixture_client' for item in artifacts):
        raise GateFailure('Normal client library graph contains the fixture test')


def test_execution(text, required_cases):
    """Read stable libtest pretty output, including sequential --nocapture logs.

    A zero exit code alone cannot establish that selected tests were executed.
    Required names are a checked-in coverage contract, not inferred from whatever
    names happen to remain in this build. Never silently regenerate the inventory.
    """
    passed = []
    pending = None
    summaries = []
    for line in text.splitlines():
        match = re.match(r'^test ([A-Za-z0-9_:]+) \.\.\. (.*)$', line)
        if match:
            name, suffix = match.groups()
            pending = name
            if suffix == 'ok':
                passed.append(name)
                pending = None
            elif suffix.startswith(('ignored', 'FAILED')):
                pending = None
        elif line == 'ok' and pending is not None:
            passed.append(pending)
            pending = None
        match = re.match(r'^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', line)
        if match:
            summaries.append(dict(zip(('passed', 'failed', 'ignored'), map(int, match.groups()))))
    required = list(required_cases or [])
    missing = sorted(set(required) - set(passed))
    # Exact named execution totals must agree with libtest's returned summaries.
    # Workspace runs may contain ignored tests or empty secondary binaries.
    total = sum(summary['passed'] for summary in summaries)
    valid = bool(summaries) and total > 0 and total == len(passed) and not missing and all(summary['failed'] == 0 for summary in summaries)
    if required:
        valid = valid and len(passed) == len(set(passed)) and all(summary['ignored'] == 0 for summary in summaries)
    return {'required_cases': required, 'executed_cases': passed, 'missing_cases': missing,
            'test_summaries': summaries, 'test_execution_verified': valid}


class Runner:
    def __init__(self, evidence, root=ROOT, deadline=None):
        self.evidence = evidence
        self.root = root
        self.deadline = deadline
        self.env = os.environ.copy()
        evidence.mkdir(parents=True, exist_ok=True)
        self.env['THESEUS_HOST_EVIDENCE'] = str(evidence)

    def run(self, name, command, timeout=1800, stdout_only=False, required_cases=None, require_tests=False):
        log = self.evidence / f'{name}.log'
        receipt = {'case': name, 'command': command, 'pass': False, 'status': 'not_run', 'exit_code': None,
                   'required_cases': list(required_cases or []), 'test_execution_verified': False if require_tests else None}
        started = time.monotonic()
        if self.deadline is not None:
            timeout = min(timeout, self.deadline - time.time() - 15)
        console(f'THESEUS case={name} command={json.dumps(command)}', flush=True)
        if timeout <= 0:
            receipt.update(status='not_run_budget_exhausted', exit_code=124)
        else:
            try:
                with log.open('wb') as output, (self.evidence / f'{name}.stdout').open('wb') as standard_output:
                    process = subprocess.Popen(command, cwd=self.root, env=self.env, stdout=standard_output if stdout_only else output,
                                               stderr=output if stdout_only else subprocess.STDOUT, start_new_session=os.name != 'nt',
                                               creationflags=subprocess.CREATE_NEW_PROCESS_GROUP if os.name == 'nt' else 0)
                    try:
                        code = process.wait(timeout=timeout)
                        receipt.update(status='returned', exit_code=code, **{'pass': code == 0})
                    except subprocess.TimeoutExpired:
                        if os.name == 'nt':
                            subprocess.run(['taskkill', '/PID', str(process.pid), '/T', '/F'],
                                           stdout=output, stderr=subprocess.STDOUT, timeout=10, check=False)
                        else:
                            os.killpg(process.pid, signal.SIGKILL)
                        process.wait()
                        receipt.update(status='watchdog_failure', exit_code=124)
            except OSError as error:
                receipt.update(status='spawn_failure', exit_code=127, error=str(error))
        if receipt['pass'] and require_tests:
            checks = test_execution(log.read_text(encoding='utf-8', errors='replace'), required_cases)
            receipt.update(checks)
            if not checks['test_execution_verified']:
                receipt.update(status='required_tests_missing', **{'pass': False})
        receipt['elapsed_seconds'] = round(time.monotonic() - started, 3)
        write_json(self.evidence / f'{name}.result.json', receipt)
        if log.exists():
            # Preserve arbitrary native bytes in the file; console display is lossy.
            console(log.read_bytes().decode('utf-8', errors='replace'), end='', flush=True)
        console(f'THESEUS case={name} exit={receipt["exit_code"]} status={receipt["status"]}', flush=True)
        if not receipt['pass']:
            raise GateFailure(f'{name} failed; evidence: {self.evidence}')
        return (self.evidence / f'{name}.stdout' if stdout_only else log).read_text(encoding='utf-8', errors='replace')

    def cargo(self, name, *arguments, timeout=1800, required_cases=None):
        executing_tests = bool(arguments) and arguments[0] == 'test' and '--no-run' not in arguments
        return self.run(name, ['cargo', *arguments], timeout, required_cases=required_cases, require_tests=executing_tests)


def load_pinned(runner):
    metadata = json.loads((runner.evidence / 'native-artifacts.json').read_text(encoding='utf-8'))
    verify_artifacts(metadata)
    runner.env['THESEUS_HOST_TEST_FIXTURE'] = metadata['fixture']['path']
    runner.env['THESEUS_MCP_BINARY'] = metadata['production']['path']
    runner.env['THESEUS_MCP_FIXTURE'] = metadata['mcp_fixture']['path']
    runner.env['THESEUS_NATIVE_HELPER_BINARY'] = metadata['helper_production']['path']
    runner.env['THESEUS_NATIVE_HELPER_FIXTURE'] = metadata['helper_fixture']['path']
    runner.env['THESEUS_NATIVE_DESKTOP_HELPER_BINARY'] = metadata['helper_fixture']['path']
    runner.env['THESEUS_NATIVE_DESKTOP_BROKER_BINARY'] = metadata['desktop_broker_fixture']['path']
    for key, (_, variable) in (CATALOG_ARTIFACTS | INTEGRATION_ARTIFACTS).items():
        runner.env[variable] = metadata[key]['path']
    if platform.system() == 'Windows':
        runner.env['THESEUS_WINDOWS_DESKTOP_FIXTURE_EXE'] = metadata['native_service_base']['path']
    return metadata


def fixture(runner):
    text = runner.run('trusted-fixture-build', [sys.executable, 'tests/support/build_host_fixture.py'], stdout_only=True)
    fixture_path = pathlib.Path(text.strip())
    metadata = json.loads((fixture_path.parent / 'artifacts.json').read_text(encoding='utf-8'))
    verify_artifacts(metadata)
    write_json(runner.evidence / 'native-artifacts.json', metadata)
    load_pinned(runner)
    if os.environ.get('GITHUB_ENV'):
        with open(os.environ['GITHUB_ENV'], 'a', encoding='utf-8') as output:
            output.write(f'THESEUS_HOST_TEST_FIXTURE={metadata["fixture"]["path"]}\n')
            output.write(f'THESEUS_MCP_BINARY={metadata["production"]["path"]}\n')
            output.write(f'THESEUS_MCP_FIXTURE={metadata["mcp_fixture"]["path"]}\n')
            output.write(f'THESEUS_NATIVE_HELPER_BINARY={metadata["helper_production"]["path"]}\n')
            output.write(f'THESEUS_NATIVE_HELPER_FIXTURE={metadata["helper_fixture"]["path"]}\n')
            output.write(f'THESEUS_NATIVE_DESKTOP_HELPER_BINARY={metadata["helper_fixture"]["path"]}\n')
            output.write(f'THESEUS_NATIVE_DESKTOP_BROKER_BINARY={metadata["desktop_broker_fixture"]["path"]}\n')
            if platform.system() == 'Windows':
                output.write(f'THESEUS_WINDOWS_DESKTOP_FIXTURE_EXE={metadata["native_service_base"]["path"]}\n')
            for key, (_, variable) in (CATALOG_ARTIFACTS | INTEGRATION_ARTIFACTS).items():
                output.write(f'{variable}={metadata[key]["path"]}\n')


def feature_boundaries(runner):
    for name, arguments, enabled in (
        ('release-artifacts', ['check', '--locked', '--release', '--bin', 'theseus', '--message-format=json'], False),
        ('app-test-artifacts', ['test', '--locked', '--lib', '--no-run', '--message-format=json'], True),
    ):
        text = runner.cargo(name, *arguments)
        artifacts = []
        applications = []
        for line in text.splitlines():
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                continue
            if value.get('reason') == 'compiler-artifact' and value.get('target', {}).get('name') == 'theseus':
                applications.append(value)
            if value.get('reason') == 'compiler-artifact' and value.get('target', {}).get('name') == 'theseus_host':
                artifacts.append(value)
        if not enabled and (not applications or any(set(item['features']) & {'mcp-test-support', 'native-catalog-test-support'} for item in applications)):
            raise GateFailure('Production application unexpectedly enables fixture support')
        if not artifacts or any(('test-support' in artifact['features']) != enabled for artifact in artifacts):
            raise GateFailure(f'{name}: native test-support feature boundary violated or artifact absent')

    for name, arguments, fixture_graph in (
        ('helper-release-artifacts', ['check', '--locked', '--release', '-p', 'theseus-native-helper', '--bin', 'theseus-native-helper', '--message-format=json'], False),
        ('helper-fixture-artifacts', ['check', '--locked', '-p', 'theseus-native-helper', '--features', 'test-support', '--bin', 'theseus-native-helper-fixture', '--message-format=json'], True),
    ):
        text = runner.cargo(name, *arguments)
        artifacts = []
        for line in text.splitlines():
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                continue
            if value.get('reason') == 'compiler-artifact':
                artifacts.append(value)
        check_helper_graph(artifacts, fixture=fixture_graph)


    for name, arguments, fixture_graph in (
        ('client-release-artifacts', ['check', '--locked', '--release', '-p', 'theseus-native-client', '--lib', '--message-format=json'], False),
        ('client-fixture-artifacts', ['test', '--locked', '-p', 'theseus-native-client', '--features', 'test-support', '--test', 'fixture_client', '--no-run', '--message-format=json'], True),
    ):
        text = runner.cargo(name, *arguments)
        artifacts = []
        for line in text.splitlines():
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                continue
            if value.get('reason') == 'compiler-artifact':
                artifacts.append(value)
        check_client_graph(artifacts, fixture=fixture_graph)


    arguments = ['check', '--locked', '--features', 'native-catalog-test-support']
    for target, _ in CATALOG_ARTIFACTS.values():
        arguments += ['--bin', target]
    arguments += ['--message-format=json']
    text = runner.cargo('catalog-fixture-artifacts', *arguments)
    artifacts = []
    for line in text.splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if value.get('reason') == 'compiler-artifact':
            artifacts.append(value)
    check_catalog_graph(artifacts)


def native(runner, stage):
    if stage == 'fixture':
        fixture(runner)
        return
    metadata = load_pinned(runner)
    py = sys.executable
    windows = platform.system() == 'Windows'
    if platform.system() == 'Darwin':
        runner.env['TMPDIR'] = str(pathlib.Path(tempfile.gettempdir()).resolve())
    if stage == 'stdio':
        runner.run('validation-driver-controls', [py, 'tests/support/validation_test.py'])
        runner.run('stdio-driver-controls', [py, 'tests/support/mcp_stdio_preflight.py', '--self-test'])
        runner.run('server-supervisor-controls', [py, 'tests/support/mcp_server_stdio.py', '--self-test'])
        runner.cargo('stdio-rust', 'test', '--locked', '-p', 'theseus-host', '--features', 'test-support', '--test', 'stdio_preflight', '--', '--nocapture', timeout=180, required_cases=CASE_INVENTORY['native']['stdio-rust'])
        runner.cargo('stdio-native-identity', 'test', '--locked', '-p', 'theseus-host', '--features', 'test-support', '--test', 'stdio', 'pipe', '--', '--nocapture', timeout=180, required_cases=CASE_INVENTORY['native']['stdio-identity-common'] + (CASE_INVENTORY['native']['stdio-identity-windows'] if windows else []))
        runner.run('stdio-python-createpipe', [py, 'tests/support/mcp_stdio_preflight.py', '--fixture', runner.env['THESEUS_HOST_TEST_FIXTURE'], '--evidence', str(runner.evidence)], timeout=180)
        runner.cargo('mcp-server-stdio', 'test', '--locked', '--features', 'mcp-test-support', '--test', 'mcp_server_stdio', '--', '--nocapture', timeout=300, required_cases=CASE_INVENTORY['native']['server-common'] + ([] if windows else CASE_INVENTORY['native']['server-unix']))
    elif stage == 'fmt':
        runner.cargo('format', 'fmt', '--all', '--check')
    elif stage == 'clippy':
        runner.cargo('clippy', 'clippy', '--locked', '--workspace', '--all-targets', '--', '-D', 'warnings')
    elif stage == 'portable':
        runner.run('python-version', [py, '--version'])
        runner.run('portable-guest-bytes', [py, 'tests/support/portable_guest_bytes.py'])
        runner.run('generated-example-controls', [py, 'tests/support/export_native_examples_test.py'])
        template = runner.root / 'tests/fixtures/recipes/v4_downgrade.toml'
        raw = template.read_bytes()
        (runner.evidence / 'engine-downgrade-template.raw').write_bytes(raw)
        write_json(runner.evidence / 'engine-template-bytes.json', {'sha256': sha256(template), 'byte_count': len(raw), 'lf_bytes': raw.count(b'\n'), 'crlf_pairs': raw.count(b'\r\n'), 'bare_lf': raw.count(b'\n') - raw.count(b'\r\n')})
    elif stage == 'features':
        feature_boundaries(runner)
    elif stage == 'precompile':
        runner.cargo('native-host-precompile', 'test', '--locked', '-p', 'theseus-host', '--features', 'test-support', '--lib', '--no-run')
        runner.cargo('native-integration-precompile', 'test', '--locked', '--test', 'host_cli', '--test', 'host_terminal', '--no-run')
        runner.cargo('native-helper-precompile', 'test', '--locked', '-p', 'theseus-native-helper', '--features', 'test-support', '--test', 'fixture_runtime', '--no-run', timeout=180)
        runner.cargo('native-client-precompile', 'test', '--locked', '-p', 'theseus-native-client', '--features', 'test-support', '--test', 'fixture_client', '--no-run', timeout=240)
        runner.cargo('native-profile-precompile', 'test', '--locked', '-p', 'theseus-native-helper', '--test', 'native_profile_runtime', '--no-run', timeout=180)
        runner.cargo('native-catalog-precompile', 'test', '--locked', '--features', 'native-catalog-test-support', '--test', 'native_catalog_adapter', '--test', 'native_selected', '--test', 'native_recovery', '--no-run', timeout=240)
        runner.cargo('native-consumers-precompile', 'test', '--locked', '--test', 'native_consumers', '--no-run', timeout=180)
        runner.run('model-action-controls', ['node', 'tests/support/observe_conpty_model_action_test.cjs'])
        if windows:
            runner.run('model-observer-controls', ['pwsh', '-NoProfile', '-File', 'tests/support/observe_conpty_model_test.ps1'])
    elif stage == 'early-controls':
        original_trace = runner.env.get('THESEUS_TERMINAL_TRACE')
        runner.env['THESEUS_TERMINAL_TRACE'] = '1'
        try:
            test = 'conpty_legacy_crossterm' if windows else 'pty_resize_and_single_q'
            runner.cargo('early-native-controls', 'test', '--locked', '-p', 'theseus-host', '--features', 'test-support', '--lib', test, '--', '--nocapture', '--test-threads=1', timeout=45, required_cases=CASE_INVENTORY['native']['early-windows' if windows else 'early-unix'])
        finally:
            if original_trace is None:
                runner.env.pop('THESEUS_TERMINAL_TRACE', None)
            else:
                runner.env['THESEUS_TERMINAL_TRACE'] = original_trace
    elif stage == 'early-cli':
        original_trace = runner.env.get('THESEUS_TERMINAL_TRACE')
        runner.env['THESEUS_TERMINAL_TRACE'] = '1'
        try:
            runner.cargo('early-native-cli-terminal', 'test', '--locked', '--test', 'host_cli', '--test', 'host_terminal', '--', '--nocapture', '--test-threads=1', timeout=165, required_cases=CASE_INVENTORY['native']['cli-terminal'])
        finally:
            if original_trace is None:
                runner.env.pop('THESEUS_TERMINAL_TRACE', None)
            else:
                runner.env['THESEUS_TERMINAL_TRACE'] = original_trace
    elif stage == 'runtime':
        runner.cargo('workspace-tests', 'test', '--locked', '--workspace', '--all-targets', '--no-fail-fast')
        runner.cargo('native-cli-terminal', 'test', '--locked', '--test', 'host_cli', '--test', 'host_terminal', '--', '--nocapture', required_cases=CASE_INVENTORY['native']['cli-terminal'])
        test = 'conpty_' if windows else 'pty_master_'
        runner.cargo('native-explicit-terminal-controls', 'test', '--locked', '-p', 'theseus-host', '--features', 'test-support', '--lib', test, '--', '--nocapture', '--test-threads=1', timeout=120, required_cases=CASE_INVENTORY['native']['runtime-windows' if windows else 'runtime-unix'])
        runner.cargo('native-helper-runtime', 'test', '--locked', '-p', 'theseus-native-helper', '--features', 'test-support', '--test', 'fixture_runtime', '--', '--nocapture', '--test-threads=1', timeout=240, required_cases=CASE_INVENTORY['native']['helper-runtime-common'] + (CASE_INVENTORY['native']['helper-runtime-windows'] if windows else []))
        runner.cargo('native-client-runtime', 'test', '--locked', '-p', 'theseus-native-client', '--features', 'test-support', '--test', 'fixture_client', '--', '--nocapture', '--test-threads=1', timeout=240, required_cases=CASE_INVENTORY['native']['client-common'] + (CASE_INVENTORY['native']['client-windows'] if windows else []))
        runner.cargo('native-profile-runtime', 'test', '--locked', '-p', 'theseus-native-helper', '--test', 'native_profile_runtime', '--', '--nocapture', '--test-threads=1', timeout=240, required_cases=CASE_INVENTORY['native']['profile-runtime-common'])
        runner.cargo('native-catalog-library', 'test', '--locked', '--features', 'native-catalog-test-support', '--lib', '--', '--nocapture', timeout=300, required_cases=CASE_INVENTORY['native']['catalog-library-common'])
        runner.cargo('native-catalog-adapter', 'test', '--locked', '--features', 'native-catalog-test-support', '--test', 'native_catalog_adapter', '--', '--nocapture', '--test-threads=1', timeout=240, required_cases=CASE_INVENTORY['native']['catalog-common'])
        runner.cargo('native-selected', 'test', '--locked', '--features', 'native-catalog-test-support', '--test', 'native_selected', '--', '--nocapture', '--test-threads=1', timeout=300, required_cases=CASE_INVENTORY['native']['selected-common'])
        runner.cargo('native-recovery', 'test', '--locked', '--features', 'native-catalog-test-support', '--test', 'native_recovery', '--', '--nocapture', '--test-threads=1', timeout=300, required_cases=CASE_INVENTORY['native']['recovery-common'])
        runner.cargo('native-consumers', 'test', '--locked', '--test', 'native_consumers', '--', '--nocapture', '--test-threads=1', timeout=180, required_cases=CASE_INVENTORY['native']['consumers-common'] + ([] if windows else CASE_INVENTORY['native']['consumers-unix']))
        metadata = load_pinned(runner)
        write_json(runner.evidence / 'native-artifacts-after.json', metadata)
    elif stage == 'integration':
        verify_integration_tests(metadata)
        current = source_identity(runner.root)
        if any(metadata.get(key) != value for key, value in current.items()):
            raise GateFailure('Source changed after integration artifact compilation')
        grammar = (
            'native_supplied_receipt_requires_explicit_platform_layout_and_instance',
            'native_supplied_receipt_preserves_exact_launchd_authority_and_rejects_implicit_inputs',
            'native_acl_policy_refuses_inherited_extra_or_content_write_executable_grants',
        )
        for case in grammar:
            runner.run('integration-' + case, [metadata['native_services_platforms']['path'], case, '--exact', '--nocapture'], required_cases=[case], require_tests=True)
        if platform.system() == 'Linux':
            cases = [
                'source_desktop_clean_uses_original_broker_child_and_public_cli_saved_views',
                'source_desktop_missing_and_leaked_entries_fail_exact_sut_postconditions',
                'source_desktop_fresh_six_tool_mcp_matches_own_saved_desktop_and_tui',
                'source_desktop_upgrade_keeps_original_base_and_target_generations',
                'source_desktop_reinstall_same_tuple_requires_new_original_sut_generation',
                'source_desktop_ordinary_fresh_six_tool_mcp_keeps_exact_generations_and_saved_views',
            ]
            runner.run('source-desktop-product', [metadata['native_desktop']['path'], 'source_desktop_', '--nocapture', '--test-threads=1'], timeout=600, required_cases=cases, require_tests=True)
        manifest = manager_manifest(metadata, runner.host, platform.system())
        write_json(runner.evidence / 'native-manager-manifest.json', manifest)
    elif stage == 'release':
        runner.cargo('workspace-release', 'build', '--locked', '--workspace', '--release')
    else:
        raise GateFailure(f'Unknown native stage: {stage}')


def docker(runner, stage):
    if platform.system() != 'Linux':
        raise GateFailure('Real Docker acceptance requires native Linux; other native OS evidence is separate')
    if stage == 'prepare':
        for suite in DOCKER_SUITES:
            directory = runner.evidence / suite
            directory.mkdir(exist_ok=True)
            write_json(directory / 'result.json', {'suite': suite, 'status': 'not_run_preparation', 'pass': False})
        runner.run('docker-info', ['docker', 'info'])
        runner.run('docker-pull', ['docker', 'pull', 'ubuntu:24.04'])
        image = runner.run('docker-image', ['docker', 'image', 'inspect', '--format', '{{.Id}}', 'ubuntu:24.04']).strip()
        if not image.startswith('sha256:') or len(image) != 71 or any(c not in '0123456789abcdef' for c in image[7:]):
            raise GateFailure('Docker did not identify the exact immutable image')
        (runner.evidence / 'docker-image-id.txt').write_text(image + '\n', encoding='ascii')
        fixture(runner)
        arguments = ['test', '--locked', '--no-run']
        for suite in DOCKER_SUITES:
            arguments.extend(['--test', suite])
        runner.cargo('docker-precompile', *arguments)
    elif stage == 'run':
        load_pinned(runner)
        runner.env['THESEUS_MCP_TEST_IMAGE'] = (runner.evidence / 'docker-image-id.txt').read_text(encoding='ascii').strip()
        failures = []
        for suite in DOCKER_SUITES:
            directory = runner.evidence / suite
            suite_runner = Runner(directory, runner.root, runner.deadline)
            suite_runner.env = runner.env.copy()
            suite_runner.env.update(THESEUS_ACCEPTANCE_TARGET=suite, THESEUS_ACCEPTANCE_EVIDENCE=str(directory), THESEUS_MCP_TEST_EVIDENCE=str(directory / 'mcp-transport'))
            try:
                suite_runner.cargo('tests', 'test', '--locked', '--test', suite, '--', '--ignored', '--test-threads=1', timeout=2685, required_cases=CASE_INVENTORY['docker'][suite])
            except GateFailure:
                failures.append(suite)
            receipt = json.loads((directory / 'tests.result.json').read_text(encoding='utf-8'))
            write_json(directory / 'result.json', dict(receipt, suite=suite))
            if receipt['exit_code'] in (124, 137):
                for pending in DOCKER_SUITES[DOCKER_SUITES.index(suite) + 1:]:
                    write_json(runner.evidence / pending / 'result.json', {'suite': pending, 'status': 'not_run_after_watchdog', 'pass': False})
                break
        load_pinned(runner)
        if failures:
            raise GateFailure(f'Required Docker acceptance failed: {failures}')
    else:
        raise GateFailure(f'Unknown Docker stage: {stage}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('native', 'docker'))
    parser.add_argument('--stage', help='CI checkpoint; omit to run the complete mode')
    parser.add_argument('--evidence', type=pathlib.Path)
    parser.add_argument('--expected-host', help='Require this exact native rustc host triple')
    parser.add_argument('--budget-start', type=float, help='Docker shared 70-minute clock start, Unix seconds')
    args = parser.parse_args()
    evidence = (args.evidence or pathlib.Path(tempfile.mkdtemp(prefix=f'theseus-{args.mode}-validation-'))).resolve()
    if evidence.is_relative_to(ROOT):
        parser.error('Evidence must be outside the source tree so receipts cannot change its identity')
    runner = Runner(evidence, deadline=(args.budget_start or time.time()) + 4200 if args.mode == 'docker' else None)
    console(f'THESEUS evidence={evidence}', flush=True)
    receipt = {'mode': args.mode, 'stage': args.stage or 'all', 'pass': False,
               'native_os': platform.system(), 'native_os_version': platform.version(),
               'native_machine': platform.machine(), 'other_native_operating_systems': 'NOT_RUN',
               'cross_compilation_is_native_acceptance': False}
    try:
        identity = source_identity(ROOT)
        receipt.update(identity)
        prior = evidence / 'source.json'
        if prior.exists() and json.loads(prior.read_text(encoding='utf-8')) != identity:
            raise GateFailure('Source changed during validation; use a fresh evidence directory and rerun')
        write_json(prior, identity)
        rustc = runner.run('rustc', ['rustc', '-Vv'])
        receipt['compiler_host'] = check_native_host(rustc, platform.system(), args.expected_host)
        runner.host = receipt['compiler_host']
        if args.mode == 'native':
            if args.stage and args.stage not in NATIVE_STAGES:
                raise GateFailure('Unknown native stage')
            for stage in (args.stage,) if args.stage else NATIVE_STAGES:
                native(runner, stage)
        else:
            for stage in (args.stage,) if args.stage else ('prepare', 'run'):
                docker(runner, stage)
        if source_identity(ROOT) != identity:
            raise GateFailure('Source changed during validation; results do not identify a single source tree')
        receipt['pass'] = True
    except (GateFailure, OSError, ValueError, subprocess.SubprocessError) as error:
        receipt['error'] = str(error)
        console(f'THESEUS FAILED: {error}', file=sys.stderr)
    finally:
        write_json(evidence / f'{args.mode}-{args.stage or "all"}.result.json', receipt)
    return 0 if receipt['pass'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
