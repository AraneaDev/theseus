"""Opt-in actual-native public CLI acceptance. No discovery or replay fallback."""
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import tomllib

from docker_absence import require_container_absent

REPO = Path(__file__).resolve().parents[2]
IMAGE = 'sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55'
TARGET = 'theseus-probe@theseus-probe-market'
COMPANION = 'theseus-companion@theseus-probe-market'


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def prerequisites():
    for name in ('THESEUS_PLUGIN_CLIENT', 'THESEUS_PLUGIN_CLIENT_VERSION'):
        assert os.environ.get(name), f'Native acceptance prerequisite: explicitly set {name}; no discovery/replay fallback'
    client = Path(os.environ['THESEUS_PLUGIN_CLIENT']).absolute()
    assert client.is_file() and not client.is_symlink(), 'explicit regular nonsymlink ELF required'
    with client.open('rb') as stream:
        assert stream.read(4) == b'\x7fELF', 'native acceptance requires an ELF artifact'
    version = os.environ['THESEUS_PLUGIN_CLIENT_VERSION']
    image = subprocess.run(['docker', 'image', 'inspect', IMAGE, '--format', '{{.Id}}'],
                           capture_output=True, text=True, check=True, timeout=10).stdout.strip()
    assert image == IMAGE
    return client, version


def attempt():
    parent = REPO / 'docs/superpowers/reviews/2026-10-30-plugin-lifecycle'
    parent.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix='native-', dir=parent))
    print('Native acceptance evidence:', root, flush=True)
    return root


def command(binary, evidence, name, args):
    argv = [binary, '--no-animation', *map(str, args)]
    (evidence / (name + '.command.json')).write_text(json.dumps(argv))
    result = subprocess.run(argv, capture_output=True, timeout=180)
    (evidence / (name + '.stdout')).write_bytes(result.stdout)
    (evidence / (name + '.stderr')).write_bytes(result.stderr)
    return result


def inspect_live(root, process, evidence, name, previous):
    """Inspect only a newly journaled owned resource; never a prefix/global target."""
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        for path in (root / '.theseus-runs').glob('*/journal.json'):
            if path in previous:
                continue
            journal = json.loads(path.read_text())
            for resource in journal['resources']:
                result = subprocess.run(['docker', 'inspect', resource['container_id']],
                                        capture_output=True, text=True, timeout=10)
                if result.returncode:
                    continue
                value = json.loads(result.stdout)[0]
                assert all(value['Config']['Labels'].get(k) == v for k, v in resource['labels'].items())
                assert value['HostConfig']['NetworkMode'] == 'none'
                assert value['Image'] == IMAGE
                assert not value['Mounts'], 'no host configuration/credential mounts'
                environment = dict(item.split('=', 1) for item in value['Config']['Env'])
                assert set(environment) <= {'PATH', 'HOME', 'HOSTNAME', 'TERM'}, environment.keys()
                (evidence / (name + '-live-inspect.json')).write_text(result.stdout)
                return
        assert process.poll() is None, 'CLI exited before runtime isolation inspection'
        time.sleep(.05)
    raise AssertionError('deadline inspecting newly owned native guest')


def native_run(binary, evidence, name, root, recipe):
    argv = [binary, '--no-animation', 'run', '--headless', '--recipe', str(recipe),
            '--profile', 'user', '--scenario', 'plugin-lifecycle']
    (evidence / (name + '.command.json')).write_text(json.dumps(argv))
    previous = set((root / '.theseus-runs').glob('*/journal.json'))
    process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    stdout, stderr = b'', b''
    try:
        inspect_live(root, process, evidence, name, previous)
        stdout, stderr = process.communicate(timeout=180)
    finally:
        if process.poll() is None:
            process.send_signal(signal.SIGINT)
            stdout, stderr = process.communicate(timeout=30)
        elif not stdout and not stderr:
            stdout, stderr = process.communicate()
        (evidence / (name + '.stdout')).write_bytes(stdout)
        (evidence / (name + '.stderr')).write_bytes(stderr)
    return process.returncode, stdout, stderr


def prepare(evidence, client, version, scope):
    root = evidence / scope
    argv = ['python3', str(REPO / 'examples/plugins/prepare.py'), '--client', str(client),
            '--client-version', version, '--scope', scope, '--output', str(root), '--image', IMAGE]
    (evidence / (scope + '-prepare.command.json')).write_text(json.dumps(argv))
    result = subprocess.run(argv, capture_output=True, timeout=60)
    (evidence / (scope + '-prepare.stdout')).write_bytes(result.stdout)
    (evidence / (scope + '-prepare.stderr')).write_bytes(result.stderr)
    assert result.returncode == 0, result.stderr
    return root


def stages(report):
    return report['scenarios'][0]['stages'] if report['scenarios'] else []


def commands(report):
    return [c for s in stages(report) for c in s['commands']]


def checks(report):
    return [c for s in stages(report) for c in s['checks']]


def verify_common(binary, evidence, name, path, expected, scope, version, sha, size):
    result = command(binary, evidence, name + '-readback', ['report', path.parent, '--json'])
    # report --json reports read/validation success; outcome remains in the document.
    assert result.returncode == 0, (result.returncode, result.stderr)
    report = json.loads(result.stdout)
    assert report['schema_version'] == 7 and report['outcome'] == expected, report['outcome']
    assert not report['errors'], report['errors']
    assert report['image_id'] == IMAGE
    assert report['network'] == 'none'
    contract = report['scenarios'][0]['plugin']
    assert contract['scope'] == scope and contract['client_version'] == version
    artifact = next(a for a in report['artifacts'] if a['name'] == 'target')
    file = next(f for f in artifact['files'] if f['path'] == 'client/claude')
    assert file['sha256'] == sha and file['size'] == size, file
    scenario = report['scenarios'][0]
    assert scenario['teardown'] == {'status': 'complete'}, scenario['teardown']
    container = scenario['container_id']
    inspection = subprocess.run(['docker', 'inspect', container], capture_output=True, text=True, timeout=10)
    (evidence / (name + '-inspect.stderr')).write_text(inspection.stderr)
    require_container_absent(container, inspection)
    journal = json.loads((path.parent / 'journal.json').read_text())
    (evidence / (name + '-journal.json')).write_text(json.dumps(journal, indent=2))
    assert journal['resources'] and all(r['removed'] for r in journal['resources']), journal
    for cmd in commands(report):
        assert cmd['identity']['uid'] == 10001 and cmd['identity']['gid'] == 10001
        assert not cmd.get('forwarded_environment', [])
    source = tomllib.loads(Path(report['recipe_path']).read_text())
    assert source['network'] == 'none' and list(source['profiles']) == ['user']
    for item in [*source['profiles']['user']['steps'].values(), *source['profiles']['user']['checks']]:
        assert not item.get('pass_env', [])
    version_proof = next(c for c in checks(report) if c['id'] == 'client-version')
    assert version_proof['outcome'] == 'passed'
    assert version_proof['json_expectation']['observations'][0]['actual'] == {'status': 'scalar', 'value': version}
    print(name, expected, path, flush=True)
    return report


def variant(root, name):
    """New recipe, same immutable explicitly staged input; no extra native copies."""
    text = (root / 'theseus.toml').read_text()
    text = text.replace('name = "Local Claude plugin lifecycle (user)"',
                        'name = "Native TEST CORRUPTION ' + name + '"')
    upgrade = 'sh /inputs/target/lifecycle.sh upgrade user'
    if name in ('timeout', 'cancel'):
        wrapper = 'set -eu; printf "TEST ONLY pre-update wait\\n"; : > /workspace/native-update-waiting; sleep 30; ' + upgrade
        text = text.replace('script = ' + json.dumps(upgrade) + '\ntimeout_seconds = 45',
                            'script = ' + json.dumps(wrapper) + '\ntimeout_seconds = ' + ('2' if name == 'timeout' else '45'))
    elif name == 'missing-component':
        wrapper = upgrade + '; rm /home/theseus/plugin-config/plugins/cache/theseus-probe-market/theseus-probe/2.0.0/skills/check/SKILL.md'
        text = text.replace('script = ' + json.dumps(upgrade), 'script = ' + json.dumps('set -eu; ' + wrapper))
    elif name == 'settings-loss':
        wrapper = upgrade + "; settings=/home/theseus/plugin-config/settings.json; tr -d '\\n' < \"$settings\" | sed 's/\"THESEUS_UNRELATED\": *\"keep-me\", *//' > /workspace/lost-setting.json; cp /workspace/lost-setting.json \"$settings\""
        text = text.replace('script = ' + json.dumps(upgrade), 'script = ' + json.dumps('set -eu; ' + wrapper))
    elif name == 'duplicate-inventory':
        # Deliberate test corruption of this known native flat-record output.
        # Retain the complete original document on stderr, including unrelated entries.
        wrapper = r'''set -eu
sh /inputs/target/lifecycle.sh inventory user > /workspace/native-original.json
cat /workspace/native-original.json >&2
awk '/^  \{/ {s=$0 "\n"; take=0; next}
 /^    "id":/ {if ($0 ~ /"theseus-probe@theseus-probe-market"/) take=1}
 {s=s $0 "\n"}
 /^  }/ {if(take){sub(/,\n$/, "\n", s); printf "%s", s; n++}; take=0}
 END {if(n!=1)exit 65}' /workspace/native-original.json > /workspace/native-target.json
sed '$d' /workspace/native-original.json
printf ',\n'
cat /workspace/native-target.json
printf ']\n'
'''
        start = text.index('id = "target-inventory"')
        end = text.index('[[profiles.user.checks]]', start)
        section = text[start:end].replace('script = "sh /inputs/target/lifecycle.sh inventory user"',
                                           'script = ' + json.dumps(wrapper))
        text = text[:start] + section + text[end:]
    else:
        raise AssertionError(name)
    path = root / (name + '.toml')
    assert not path.exists()
    path.write_text(text)
    return path


def wrapper_waiting(report, evidence):
    scenario = report['scenarios'][0]
    container = scenario['container_id']
    argv = ['docker', 'exec', '--user', '10001:10001', container, '/bin/sh', '-c',
            'test -f /workspace/native-update-waiting']
    result = subprocess.run(argv, capture_output=True, timeout=5)
    if result.returncode == 0:
        (evidence / 'cancel-wrapper-observed.json').write_text(json.dumps({
            'argv': argv, 'exit': result.returncode, 'container_id': container}))
        return True
    return False


def wait_for_upgrade(root, process, evidence):
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        for path in (root / '.theseus-runs').glob('*/report.json'):
            try:
                report = json.loads(path.read_text())
            except (OSError, ValueError):
                continue
            if any(s['name'] == 'upgrade' and not s['commands'] for s in stages(report)):
                if wrapper_waiting(report, evidence):
                    return
        assert process.poll() is None, 'CLI exited before cancellable update'
        time.sleep(.05)
    raise AssertionError('deadline waiting for native pre-update wrapper')


def run_fault(binary, evidence, root, name, version, sha, size):
    recipe = variant(root, name)
    argv = ['run', '--headless', '--recipe', recipe, '--profile', 'user', '--scenario', 'plugin-lifecycle']
    if name == 'cancel':
        full = [binary, '--no-animation', *map(str, argv)]
        (evidence / (name + '.command.json')).write_text(json.dumps(full))
        previous = set((root / '.theseus-runs').glob('*/journal.json'))
        process = subprocess.Popen(full, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            inspect_live(root, process, evidence, name, previous)
            wait_for_upgrade(root, process, evidence)
            process.send_signal(signal.SIGINT)
            stdout, stderr = process.communicate(timeout=30)
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGTERM)
                process.communicate(timeout=15)
        (evidence / (name + '.stdout')).write_bytes(stdout)
        (evidence / (name + '.stderr')).write_bytes(stderr)
        code = process.returncode
    else:
        code, stdout, stderr = native_run(binary, evidence, name, root, recipe)
    expected = {'cancel': 'cancelled', 'timeout': 'incomplete'}.get(name, 'failed')
    assert code == {'failed': 1, 'incomplete': 2, 'cancelled': 130}[expected], (code, stdout, stderr)
    path = Path(stdout.decode().split('Report: ', 1)[1].splitlines()[0])
    report = verify_common(binary, evidence, name, path, expected, 'user', version, sha, size)
    assert all(c['outcome'] == 'passed' for c in checks(report) if c['checkpoint'] == 'base_installed')
    update = next(c for c in commands(report) if c['phase'] == 'upgrade')
    if name in ('timeout', 'cancel'):
        assert update['timed_out'] == (name == 'timeout') and update['cancelled'] == (name == 'cancel')
        assert commands(report)[-1]['phase'] == 'upgrade', 'no later client/check command may execute'
        assert 'TEST ONLY pre-update wait' in (path.parent / update['stdout']['path']).read_text()
        assert not any(c['phase'] in ('disable_plugin', 'enable_plugin', 'uninstall') for c in commands(report))
    else:
        assert update['exit_status'] == 0, 'fault must follow successful native update'
        proof_id = {'duplicate-inventory': 'target-inventory', 'settings-loss': 'target-scope-settings',
                    'missing-component': 'target-components'}[name]
        proof = next(c for c in checks(report) if c['id'] == proof_id)
        assert proof['outcome'] == 'failed', proof
        if name == 'duplicate-inventory':
            assert proof['json_expectation']['observations'][0]['matched_count'] == 2
            producer = next(c for c in commands(report) if c['phase'] == 'check:target-inventory')
            original = json.loads((path.parent / producer['stderr']['path']).read_text())
            corrupted = json.loads((path.parent / producer['stdout']['path']).read_text())
            target = next(v for v in original if v['id'] == TARGET)
            assert corrupted == original + [target], 'corruption must preserve all original entries'
            assert sum(v['id'] == COMPANION for v in corrupted) == 1


def run_cli(binary):
    client, version = prerequisites()
    evidence = attempt()
    sha, size = digest(client), client.stat().st_size
    (evidence / 'source-identity.json').write_text(json.dumps({'path': str(client), 'sha256': sha,
        'size': size, 'expected_version': version, 'image_id': IMAGE}, indent=2))
    for scope in ('user', 'project', 'local'):
        root = prepare(evidence, client, version, scope)
        recipe = root / 'theseus.toml'
        result = command(binary, evidence, scope + '-validate', ['validate', '--recipe', recipe])
        assert result.returncode == 0, result.stderr
        code, stdout, stderr = native_run(binary, evidence, scope + '-healthy', root, recipe)
        assert code == 0, (stdout, stderr)
        path = Path(stdout.decode().split('Report: ', 1)[1].splitlines()[0])
        report = verify_common(binary, evidence, scope + '-healthy', path, 'passed', scope, version, sha, size)
        assert [s['checkpoint'] for s in stages(report) if s['checkpoint']] == [
            'base_installed', 'target_installed', 'plugin_disabled', 'plugin_enabled', 'uninstalled']
        assert all(c['outcome'] == 'passed' for c in checks(report))
        assert len(checks(report)) == 19
    root = evidence / 'user'
    for name in ('duplicate-inventory', 'settings-loss', 'missing-component', 'timeout', 'cancel'):
        run_fault(binary, evidence, root, name, version, sha, size)
    assert digest(client) == sha and client.stat().st_size == size, 'source artifact changed'
    (evidence / 'PASS').write_text('native all-scopes public CLI, three deliberate faults, timeout/cancel passed\n')


if __name__ == '__main__':
    assert len(sys.argv) == 3
    binary = str(Path(sys.argv[1]).resolve())
    assert sys.argv[2] == 'cli'
    run_cli(binary)
