#!/usr/bin/env python3
"""Real production serve/direct-CLI lifecycle parity; no external client or model."""
import hashlib
import json
import os
import pathlib
import shutil
import signal
import subprocess
import sys
import time

sys.dont_write_bytecode = True
from mcp_server_stdio import Server


def read_json(path):
    return json.loads(path.read_text(encoding='utf-8'))


def wait_for(predicate, label, timeout=90):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError(f'watchdog: {label}')


def report_file(output):
    reports = list(output.glob('*/report.json'))
    assert len(reports) <= 1, reports
    return reports[0] if reports else None


def installer_waiting(output, docker):
    path = report_file(output)
    if path is None:
        return False
    report = read_json(path)
    for scenario in report['scenarios']:
        identifier = scenario.get('container_id')
        if not identifier:
            continue
        result = subprocess.run([docker, 'exec', identifier, '/bin/sh', '-c', 'test -f /workspace/child.pid'],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5, check=False)
        if result.returncode == 0:
            return True
    return False


def assert_absent(docker, identifier):
    result = subprocess.run([docker, 'inspect', identifier], capture_output=True, timeout=10, check=False)
    error = result.stderr.decode('utf-8', errors='replace').strip().lower()
    assert result.returncode == 1 and error in (
        f'error: no such object: {identifier}',
        f'error response from daemon: no such container: {identifier}'), (identifier, result.returncode, result.stderr)


def tool(server, name, arguments):
    result = server.tool(name, arguments)
    assert result['ok'], result
    return result


def main():
    binary, root_text, image = sys.argv[1:]
    root = pathlib.Path(root_text).resolve()
    assert sys.platform.startswith('linux'), 'Linux Docker supplements the three separate native OS gates'
    assert len(image) == 71 and image.startswith('sha256:') and all(c in '0123456789abcdef' for c in image[7:])
    docker = shutil.which('docker')
    assert docker, 'Docker is required, never skipped'
    source = pathlib.Path(__file__).resolve().parents[1] / 'fixtures/lifecycle'
    original_path = os.environ['PATH']
    cases = []
    for case, fault in [('success', ''), ('command-failure', 'install'),
                        ('cancellation', 'cancellation'), ('teardown-recovery', '')]:
        case_root = root / case
        case_root.mkdir(parents=True)
        project = case_root / 'project'
        shutil.copytree(source, project)
        text = (project / 'recipe.toml').read_text(encoding='utf-8').replace('ubuntu:24.04', image)
        text = text.replace('THESEUS_FIXTURE_FAULT = ""', f'THESEUS_FIXTURE_FAULT = "{fault}"')
        (project / 'recipe.toml').write_bytes(text.encode('utf-8'))
        proxy = case_root / 'bin'
        proxy.mkdir()
        calls = case_root / 'docker-calls.jsonl'
        wrapper = proxy / 'docker'
        wrapper.write_text(f'''#!{sys.executable}
import json,os,sys
with open({str(calls)!r},'a',encoding='utf-8') as output: output.write(json.dumps(sys.argv[1:])+'\\n')
if {case == 'teardown-recovery'!r} and sys.argv[1:2]==['rm']: raise SystemExit(1)
os.execv({docker!r},[{docker!r},*sys.argv[1:]])
''', encoding='utf-8')
        wrapper.chmod(0o700)
        environment = dict(os.environ, PATH=str(proxy) + os.pathsep + original_path)
        direct_output = project / '.theseus-runs'
        direct_log = (case_root / 'direct-cli.log').open('wb')
        direct = subprocess.Popen([binary, 'run', '--headless', '--recipe', str(project / 'recipe.toml'),
                                   '--profile', 'user', '--scenario', 'clean'],
                                  cwd=project, env=environment, stdout=direct_log, stderr=subprocess.STDOUT)
        try:
            if fault == 'cancellation':
                wait_for(lambda: installer_waiting(direct_output, docker), 'direct installer readiness')
                direct.send_signal(signal.SIGINT)
            direct_exit = direct.wait(timeout=90)
        finally:
            if direct.poll() is None:
                direct.kill()
                direct.wait()
            direct_log.close()
        expected_exit = {'success': 0, 'command-failure': 1, 'cancellation': 130, 'teardown-recovery': 2}[case]
        assert direct_exit == expected_exit, (case, direct_exit, (case_root / 'direct-cli.log').read_text())
        direct_report = report_file(direct_output)
        assert direct_report is not None
        # A fresh sentinel separates the direct run from server read-only qualification.
        calls.unlink()
        output = case_root / 'server-runs'
        old_path = os.environ['PATH']
        os.environ['PATH'] = environment['PATH']
        try:
            server = Server(binary, project, output, evidence=case_root / 'stdio')
        finally:
            os.environ['PATH'] = old_path
        try:
            server.request('tools/list')
            inspect = tool(server, 'recipe_inspect', {'recipe_path': 'recipe.toml'})
            instance = inspect['server_instance']
            digest = inspect['data']['recipe_digest']
            assert digest == hashlib.sha256((project / 'recipe.toml').read_bytes()).hexdigest()
            assert not tool(server, 'run_list', {'server_instance': instance})['data']['runs']
            for name, arguments in [
                ('run_status', {'server_instance': instance, 'handle': instance + ':1'}),
                ('run_evidence', {'server_instance': instance, 'handle': instance + ':1', 'selector': {'kind': 'catalog', 'collection': 'run'}}),
            ]:
                assert not server.tool(name, arguments)['ok']
            assert not calls.exists(), 'Read-only calls invoked Docker'
            arguments = {'server_instance': instance, 'request_key': case, 'recipe_path': 'recipe.toml',
                         'recipe_digest': digest, 'profiles': ['user'], 'scenarios': ['clean'], 'pull': False}
            start = tool(server, 'run_start', arguments)['data']
            handle = start['handle']
            assert tool(server, 'run_start', arguments)['data']['handle'] == handle
            if fault == 'cancellation':
                wait_for(lambda: installer_waiting(output, docker), 'server installer readiness')
                cancelled = tool(server, 'run_cancel', {'server_instance': instance, 'handle': handle})['data']
                assert cancelled['requested_now']
            def terminal():
                status = tool(server, 'run_status', {'server_instance': instance, 'handle': handle})['data']['run']
                return status if status['state'] in ('finished', 'start_error', 'worker_error') else None
            status = wait_for(terminal, 'server joined terminal state')
            assert status['state'] == 'finished', status
            assert status['engine_result'] is not None, status
            saved = tool(server, 'run_evidence', {'server_instance': instance, 'handle': handle,
                                                 'selector': {'kind': 'catalog', 'collection': 'run'}})
            assert saved['data']['records'][0]['record_id'] == 'run'
            (case_root / 'status.json').write_text(json.dumps(status, indent=2), encoding='utf-8')
            (case_root / 'evidence.json').write_text(json.dumps(saved, indent=2), encoding='utf-8')
        finally:
            server.close()
        served_report = report_file(output)
        assert served_report is not None
        journals = [read_json(path.parent / 'journal.json') for path in (direct_report, served_report)]
        recovered = []
        for path, journal in zip((direct_report, served_report), journals):
            assert journal['resources'], (case, journal)
            if case == 'teardown-recovery':
                assert any(not resource['removed'] for resource in journal['resources']), journal
                recovery = subprocess.run([binary, 'cleanup', str(path.parent)], env=dict(os.environ, PATH=original_path),
                                          capture_output=True, timeout=40, check=False)
                assert recovery.returncode == 0, (recovery.stdout, recovery.stderr)
                recovered.append(read_json(path.parent / 'journal.json'))
                assert all(resource['removed'] for resource in recovered[-1]['resources'])
            else:
                assert all(resource['removed'] for resource in journal['resources']), journal
            for resource in journal['resources']:
                assert_absent(docker, resource['container_id'])
        # Keep original failed teardown journal observations as well as recovery receipts.
        (case_root / 'journals.json').write_text(json.dumps(journals, indent=2), encoding='utf-8')
        (case_root / 'recovered-journals.json').write_text(json.dumps(recovered, indent=2), encoding='utf-8')
        cases.append({'case': case, 'direct_report': str(direct_report), 'server_report': str(served_report),
                      'status': str(case_root / 'status.json'), 'journals': str(case_root / 'journals.json')})
    (root / 'cases.json').write_text(json.dumps(cases, indent=2), encoding='utf-8')
    print(json.dumps({'cases': [case['case'] for case in cases], 'production_binary': binary,
                      'binary_sha256': hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(), 'image': image}))


if __name__ == '__main__':
    try:
        main()
    finally:
        # Failure paths still recover only this test's verified journaled resources.
        # Recovery errors remain failures; no name-prefix or unrelated cleanup.
        if len(sys.argv) == 4:
            for journal in pathlib.Path(sys.argv[2]).rglob('journal.json'):
                if any(not resource['removed'] for resource in read_json(journal)['resources']):
                    recovery = subprocess.run([sys.argv[1], 'cleanup', str(journal.parent)],
                                              capture_output=True, timeout=40, check=False)
                    assert recovery.returncode == 0, (recovery.stdout, recovery.stderr)

