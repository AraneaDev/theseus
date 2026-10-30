"""Explicit supplied-client acceptance through the public Theseus CLI; no host client execution."""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import shlex
import subprocess
import sys
import tempfile
import time
import tomllib
from docker_absence import require_container_absent

REPO = Path(__file__).resolve().parents[2]
VERSIONS = {'claude': ('claude-code-2.1.312', '2.1.312 (Claude Code)'), 'codex': ('codex-cli-0.166.0', 'codex-cli 0.166.0')}
ERAS = ('2026-07-28', '2025-11-25')
PREPARE = REPO / 'examples/mcp/prepare.py'
_spec = importlib.util.spec_from_file_location('mcp_prepare', PREPARE)
preparer = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(preparer)


def digest(path):
    sha = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            sha.update(block)
    return sha.hexdigest()


def strict_json(value):
    def pairs(items):
        result = {}
        for key, item in items:
            assert key not in result, f'duplicate key: {key}'
            result[key] = item
        return result
    return json.loads(value, object_pairs_hook=pairs, parse_constant=lambda s: (_ for _ in ()).throw(ValueError(s)))


def wire_facts(lines, era):
    """Independent response oracle, deliberately not McpEvidence::verified_pass/replay."""
    assert 2 <= len(lines) <= 9
    frames = [strict_json(line) for line in lines]
    assert all(f['jsonrpc'] == '2.0' and 'error' not in f and 'method' not in f for f in frames)
    first = frames[0]
    result = first['result']
    if era == ERAS[0]:
        assert first['id'] == 'theseus-discover-1'
        assert era in result['supportedVersions']
        identity = result['_meta']['io.modelcontextprotocol/serverInfo']
    else:
        assert first['id'] == 'theseus-initialize-1'
        assert result['protocolVersion'] == era
        identity = result['serverInfo']
    assert 'tools' in result['capabilities']
    tools, cursors = [], set()
    for index, frame in enumerate(frames):
        result = frame['result']
        if era == ERAS[0]:
            assert result['resultType'] == 'complete' and result['ttlMs'] >= 0
            assert result['cacheScope'] in ('public', 'private')
        if index == 0:
            continue
        assert type(frame['id']) is int and frame['id'] == index
        for tool in result['tools']:
            assert tool['inputSchema']['type'] == 'object'
            assert isinstance(tool['name'], str) and tool['name']
            assert tool['name'] not in tools
            tools.append(tool['name'])
        cursor = result.get('nextCursor')
        if index < len(frames) - 1:
            assert cursor and cursor not in cursors
            cursors.add(cursor)
        else:
            assert cursor is None, 'incomplete tools/list pagination'
    return {'identity': identity, 'tools': sorted(tools), 'protocol': era}


def expect_inventory(observed, version, tools):
    assert observed['identity'] == {'name': 'theseus-fixture', 'version': version}, observed
    assert observed['tools'] == sorted(tools), observed


def registration_facts(doc, contract, checkpoint, scenario):
    removed = checkpoint == 'uninstalled'
    plugin = contract['delivery'] == 'plugin'
    base = checkpoint == 'base_installed' or checkpoint == 'configured' and scenario == 'upgrade'
    launch = {'command':'/bin/sh', 'args':['/inputs/' + ('base' if base else 'target') + '/server.sh']}
    if contract['adapter'].startswith('codex'):
        targets = [v for v in doc['list'] if v['name'] == 'probe']
        assert len(targets) == (0 if removed else 1), targets
        if not removed:
            assert targets[0]['enabled'] is True
            item = doc['get']
            assert item['name'] == 'probe' and item['enabled'] is True
            transport = item['transport']
            assert transport['type'] == 'stdio'
            assert {'command':transport['command'],'args':transport['args']} == launch
    else:
        maps = [doc['client']['mcpServers'], doc['project']['mcpServers'], doc['client']['projects']['/workspace']['mcpServers']]
        slot = {'user':0,'project':1,'local':2}[contract['scope']]
        for index, mapping in enumerate(maps):
            if plugin or removed or index != slot:
                assert 'probe' not in mapping, mapping
            else:
                assert {k:mapping['probe'][k] for k in ('command','args')} == launch
        if plugin:
            targets = [v for v in doc['plugins'] if v['id'] == 'theseus-probe@theseus-probe-market']
            assert len(targets) == (0 if removed else 1), targets
            if not removed:
                item = doc['plugins'][1]
                assert item['id'] == 'theseus-probe@theseus-probe-market'
                assert item['scope'] == 'user' and item['enabled'] == (checkpoint != 'plugin_disabled')
                assert item['readFromFolder'] == '/workspace/local-market/plugins/theseus-probe'
                assert item['mcpServers']['probe'] == {'command':'/bin/sh','args':['${CLAUDE_PLUGIN_ROOT}/server.sh']}
    return {'checkpoint':checkpoint,'scope':contract['scope'],'delivery':contract['delivery'],'removed':removed}


def prerequisites():
    names = ('THESEUS_MCP_CLAUDE_CLIENT', 'THESEUS_MCP_CODEX_CLIENT', 'THESEUS_MCP_TEST_IMAGE')
    for name in names:
        assert os.environ.get(name), f'UNRUN native acceptance: explicitly set {name}; no discovery/download/replay fallback'
    clients = {}
    for kind in VERSIONS:
        path = Path(os.environ[f'THESEUS_MCP_{kind.upper()}_CLIENT']).absolute()
        assert path.is_file() and not path.is_symlink(), 'explicit regular nonsymlink ELF required'
        with path.open('rb') as stream:
            assert stream.read(4) == b'\x7fELF'
        clients[kind] = path
    image = os.environ['THESEUS_MCP_TEST_IMAGE']
    assert image.startswith('sha256:') and len(image) == 71 and all(c in '0123456789abcdef' for c in image[7:]), 'explicit immutable prepared image ID required'
    result = subprocess.run(['docker', 'image', 'inspect', image, '--format', '{{.Id}}'], capture_output=True, text=True, timeout=10)
    assert result.returncode == 0 and result.stdout.strip() == image, result.stderr
    return clients, image


def source_manifest(binary):
    # Include new test files before their first commit, not generated evidence.
    tracked = subprocess.run(['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard'], cwd=REPO,
                             capture_output=True, check=True, timeout=10).stdout.split(b'\0')
    files = {os.fsdecode(p): digest(REPO / os.fsdecode(p)) for p in tracked if p and (REPO / os.fsdecode(p)).is_file()}
    return {'head': subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=REPO, capture_output=True, text=True, check=True, timeout=10).stdout.strip(),
            'files': files, 'public_binary': {'path': str(binary), 'sha256': digest(binary)},
            'package_version': tomllib.loads((REPO / 'Cargo.toml').read_text())['package']['version']}


def attempt(binary):
    parent = REPO / '.superpowers/sdd/2026-10-30-mcp-lifecycle/native'
    parent.mkdir(parents=True, exist_ok=True)
    evidence = Path(tempfile.mkdtemp(prefix='acceptance-', dir=parent))
    manifest = source_manifest(binary)
    (evidence / 'source-manifest.json').write_text(json.dumps(manifest, indent=2))
    print('Native evidence:', evidence, flush=True)
    return evidence, manifest


def command(evidence, name, argv, seconds=180):
    argv = list(map(str, argv))
    (evidence / (name + '.argv.json')).write_text(json.dumps(argv))
    result = subprocess.run(argv, capture_output=True, timeout=seconds)
    (evidence / (name + '.stdout')).write_bytes(result.stdout)
    (evidence / (name + '.stderr')).write_bytes(result.stderr)
    return result


def health_capture(script, capture='/home/theseus/native-health.capture'):
    path = shlex.quote(capture)
    return f'status=0; {script} || status=$?; if test -f {path}; then cat {path} >&2; fi; exit "$status"'


def prepare(evidence, clients, image, kind, scope, scenario, era, delivery='standalone'):
    name = '-'.join((kind, delivery, scope, scenario, era))
    root = evidence / name
    adapter, version = VERSIONS[kind]
    result = command(evidence, name + '-prepare', ['python3', PREPARE, '--client', clients[kind], '--client-version', version,
        '--adapter', adapter, '--scope', scope, '--scenario', scenario, '--protocol', era, '--delivery', delivery,
        '--output', root, '--image', image], 60)
    assert result.returncode == 0, result.stderr
    manifest = strict_json((root / 'input-manifest.json').read_bytes())
    assert manifest['client']['sha256'] == digest(clients[kind])
    assert manifest['client']['size'] == clients[kind].stat().st_size
    # Keep the exact native health text as separate supporting stderr evidence.
    # The production adapter still derives its finite receipt and exact marker.
    value = tomllib.loads((root / 'theseus.toml').read_text())
    for check in value['profiles']['user']['checks']:
        if check['id'].endswith('-activation'):
            check['script'] = health_capture(check['script'])
    (root / 'theseus.toml').write_text('\n'.join(json.dumps(k) + ' = ' + preparer.toml_value(v) for k,v in value.items()) + '\n')
    manifest['acceptance_recipe_sha256'] = digest(root / 'theseus.toml')
    (root / 'input-manifest.json').write_text(json.dumps(manifest, indent=2))
    return root


def stages(report):
    return report['scenarios'][0]['stages'] if report['scenarios'] else []


def commands(report):
    return [c for s in stages(report) for c in s['commands']]


def checks(report):
    return [c for s in stages(report) for c in s['checks']]


def direct_ready(logs, token, era):
    expected = 'theseus-discover-1' if era == ERAS[0] else 'theseus-initialize-1'
    prefix = f'THESEUS_DIRECT_HOLD token={token} uid=10001 request='
    for path in Path(logs).glob('*-stderr.log'):
        stdout = path.with_name(path.name.removesuffix('-stderr.log') + '-stdout.log')
        if not stdout.is_file():
            continue
        for line in path.read_text(errors='replace').splitlines():
            if line.startswith(prefix):
                request = strict_json(line[len(prefix):])
                if request.get('id') == expected and request.get('method') == ('server/discover' if era == ERAS[0] else 'initialize'):
                    return {'stderr_path': str(path), 'stdout_path': str(stdout),
                            'stderr_sha256_at_ready': digest(path), 'stdout_sha256_at_ready': digest(stdout),
                            'stderr_bytes_at_ready': path.stat().st_size, 'stdout_bytes_at_ready': stdout.stat().st_size,
                            'request': request, 'uid': 10001, 'token': token}
    return None


def run_cli(binary, evidence, name, root, recipe, image, cancel=False):
    scenario = tomllib.loads(recipe.read_text())['scenarios'][0].replace('_', '-')
    argv = [str(binary), '--no-animation', 'run', '--headless', '--recipe', str(recipe), '--profile', 'user', '--scenario', scenario]
    (evidence / (name + '.argv.json')).write_text(json.dumps(argv))
    previous = set((root / '.theseus-runs').glob('*/journal.json'))
    process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    inspected, ready = False, None
    deadline = time.monotonic() + 180
    stdout, stderr = b'', b''
    try:
        while process.poll() is None:
            assert time.monotonic() < deadline, 'public run deadline'
            for journal_path in (root / '.theseus-runs').glob('*/journal.json'):
                if journal_path in previous:
                    continue
                journal = strict_json(journal_path.read_bytes())
                if not inspected and journal['resources']:
                    resource = journal['resources'][0]
                    result = subprocess.run(['docker', 'inspect', resource['container_id']], capture_output=True, timeout=10)
                    if result.returncode == 0:
                        live = strict_json(result.stdout)[0]
                        assert live['Image'] == image and live['HostConfig']['NetworkMode'] == 'none'
                        assert not live['Mounts'], 'no host config/credentials/socket mounts'
                        assert all(live['Config']['Labels'].get(k) == v for k, v in resource['labels'].items())
                        assert set(v.split('=', 1)[0] for v in live['Config']['Env']) <= {'PATH','HOME','HOSTNAME','TERM'}
                        (evidence / (name + '-live-inspect.json')).write_bytes(result.stdout)
                        inspected = True
                if cancel and ready is None:
                    ready = direct_ready(journal_path.parent / 'logs', 'acceptance-target-v2', ERAS[0])
                    if ready:
                        (evidence / (name + '-cancel-ready.json')).write_text(json.dumps(ready, indent=2))
                        process.send_signal(signal.SIGINT)
            time.sleep(.03)
        stdout, stderr = process.communicate(timeout=10)
        assert inspected, 'native run was never inspected live'
        if cancel:
            assert ready, 'cancel did not observe this direct v2 probe in hold'
    finally:
        if process.poll() is None:
            process.send_signal(signal.SIGINT)
            stdout, stderr = process.communicate(timeout=30)
        (evidence / (name + '.stdout')).write_bytes(stdout)
        (evidence / (name + '.stderr')).write_bytes(stderr)
    assert b'Report: ' in stdout, (process.returncode, stdout, stderr)
    path = Path(stdout.decode().split('Report: ', 1)[1].splitlines()[0])
    return process.returncode, path


def verify(binary, evidence, name, path, expected, image, client, era):
    readback = command(evidence, name + '-readback', [binary, '--no-animation', 'report', path.parent, '--json'])
    assert readback.returncode == 0, readback.stderr
    report = strict_json(readback.stdout)
    assert report['schema_version'] == 8 and report['outcome'] == expected, (expected, report['outcome'], path)
    assert not report['errors'], report['errors']
    assert report['image_id'] == image and report['network'] == 'none'
    scenario = report['scenarios'][0]
    assert scenario['teardown'] == {'status':'complete'}, scenario['teardown']
    contract = scenario['mcp']
    inputs = strict_json((Path(report['recipe_path']).parent / 'input-manifest.json').read_bytes())
    assert contract['scope'] == inputs['scope'] and contract['adapter'] == inputs['adapter']
    assert inputs['client']['sha256'] == digest(client) and inputs['image'] == image
    kind = 'codex' if contract['adapter'].startswith('codex') else 'claude'
    assert contract['client_version'] == VERSIONS[kind][1]
    assert contract['client_claim'] == ('configuration_only' if kind == 'codex' else 'native_health')
    artifact = next(a for a in report['artifacts'] if a['name'] == 'target')
    file = next(f for f in artifact['files'] if f['path'] == 'client/claude')
    assert file['sha256'] == digest(client) and file['size'] == client.stat().st_size
    version = next(c for c in checks(report) if c['id'] == 'mcp-client-version')
    assert version['outcome'] == 'passed', version
    assert version['json_expectation']['observations'][0]['actual']['value'] == VERSIONS[kind][1]
    journal = strict_json((path.parent / 'journal.json').read_bytes())
    assert journal['resources'] and all(r['removed'] for r in journal['resources'])
    for resource in journal['resources']:
        owned = resource['container_id']
        result = command(evidence, name + '-absent-' + owned, ['docker','inspect',owned], 10)
        require_container_absent(owned, subprocess.CompletedProcess(result.args, result.returncode,
            result.stdout.decode(), result.stderr.decode()))
    (evidence / (name + '-owned.json')).write_text(json.dumps(journal, indent=2))
    independent, registrations = [], []
    for cmd in commands(report):
        assert cmd['identity']['uid'] == 10001 and cmd['identity']['gid'] == 10001
        assert not cmd.get('forwarded_environment', [])
    for check in checks(report):
        if check['outcome'] == 'passed' and check['id'].endswith('-registration'):
            cmd = next(c for c in commands(report) if c['id'] == check['command_ids'][0])
            doc = strict_json((path.parent / cmd['stdout']['path']).read_bytes())
            registrations.append(registration_facts(doc, contract, check['checkpoint'], scenario['scenario']))
        if check['outcome'] == 'passed' and check['id'].endswith('-activation'):
            cmd = next(c for c in commands(report) if c['id'] == check['command_ids'][0])
            receipt = strict_json((path.parent / cmd['stdout']['path']).read_bytes())
            raw_health = (path.parent / cmd['stderr']['path']).read_text()
            assert cmd['exit_status'] == 0 and receipt['status'] == 0
            if receipt['connected']:
                assert f"{receipt['alias']}: {receipt['source']} - ✔ Connected" in raw_health.splitlines()
            else:
                assert not any(receipt['alias'] + ':' in line for line in raw_health.splitlines())
        if not check.get('mcp_required'):
            continue
        proof = check.get('mcp')
        if check['outcome'] != 'passed':
            continue
        assert proof and proof['protocol_complete'] and proof['stdout_complete'] and proof['shutdown'] == 'graceful' and proof['issue'] is None
        cmd = next(c for c in commands(report) if c['id'] == check['command_ids'][0])
        assert cmd['argv'] == proof['probe']['argv'] and cmd['exit_status'] == 0
        assert not cmd['stdout']['truncated']
        raw = (path.parent / cmd['stdout']['path']).read_bytes()
        assert raw == ''.join(proof['response_lines']).encode(), 'fixture stdout must be actual persisted responses'
        facts = wire_facts(raw.decode().splitlines(keepends=True), era)
        is_base = check['checkpoint'] == 'base_installed' or check['checkpoint'] == 'configured' and scenario['scenario'] == 'upgrade'
        expect_inventory(facts, '1.0.0' if is_base else '2.0.0', ['hello_v1' if is_base else 'hello_v2'])
        independent.append({'check':check['id'], 'argv':cmd['argv'], 'stdout_path':cmd['stdout']['path'], **facts})
    (evidence / (name + '-independent-responses.json')).write_text(json.dumps(independent, indent=2))
    (evidence / (name + '-independent-registration.json')).write_text(json.dumps(registrations, indent=2))
    if expected == 'passed':
        assert checks(report) and all(c['outcome'] == 'passed' for c in checks(report))
        assert len(independent) == (3 if scenario['scenario'] == 'plugin_lifecycle' else 2 if scenario['scenario'] == 'upgrade' else 1)
        if kind == 'codex':
            assert all(not p.get('activation') for p in contract['checkpoints'].values())
            assert all('activation' not in c['id'] for c in checks(report))
    print(name, expected, path, flush=True)
    return report


def fault_recipe(root, name):
    value = tomllib.loads((root / 'theseus.toml').read_text())
    profile = value['profiles']['user']
    if name in ('duplicate','stale','settings-loss'):
        upgrade = profile['steps']['upgrade']['script']
        suffix = {
            'duplicate': '/home/theseus/.local/bin/theseus-client mcp add --scope project probe -- /bin/sh /inputs/target/server.sh',
            'stale': '/home/theseus/.local/bin/theseus-client mcp remove probe --scope user; /home/theseus/.local/bin/theseus-client mcp add --scope user probe -- /bin/sh /inputs/base/server.sh',
            'settings-loss': "sed -i 's/\"hasCompletedOnboarding\": *true/\"hasCompletedOnboarding\":false/' /home/theseus/plugin-config/.claude.json",
        }[name]
        profile['steps']['upgrade']['script'] = 'set -eu; export CLAUDE_CONFIG_DIR=/home/theseus/plugin-config CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 DISABLE_AUTOUPDATER=1; ' + upgrade + '; ' + suffix
    else:
        probe = next(c for c in profile['checks'] if c['id'] == 'target_installed-probe')['probe']
        probe['env'] = {'THESEUS_NATIVE_FAULT':name, 'THESEUS_NATIVE_TOKEN':'acceptance-target-v2'}
        probe['timeout_seconds'] = 15 if name == 'cancel' else 3
    path = root / (name + '.toml')
    path.write_text('\n'.join(json.dumps(k) + ' = ' + preparer.toml_value(v) for k,v in value.items()) + '\n')
    return path


def add_test_only_server_faults(root):
    path = root / 'target/server.sh'
    original = path.read_text()
    original_sha256 = digest(path)
    prefix = '''#!/bin/sh
# TEST ONLY direct-probe faults; native health has no injected environment.
if test "${THESEUS_NATIVE_FAULT:-}" = cancel || test "${THESEUS_NATIVE_FAULT:-}" = timeout; then
 IFS= read -r request
 printf 'THESEUS_DIRECT_HOLD token=%s uid=%s request=%s\\n' "$THESEUS_NATIVE_TOKEN" "$(id -u)" "$request" >&2
 while :; do sleep 1; done
fi
if test "${THESEUS_NATIVE_FAULT:-}" = blocked-stdin; then
 printf 'THESEUS_DIRECT_BLOCKED uid=%s\\n' "$(id -u)" >&2
 while :; do sleep 1; done
fi
if test "${THESEUS_NATIVE_FAULT:-}" = malformed; then
 IFS= read -r request
 printf '{invalid\\n'
 while IFS= read -r request; do :; done
 exit 0
fi
'''
    original = original.replace('tool=hello_v2', 'tool=hello_v2\nif test "${THESEUS_NATIVE_FAULT:-}" = wrong-tools; then tool=actually_wrong; fi')
    original = original.replace('version=2.0.0', 'version=2.0.0\nif test "${THESEUS_NATIVE_FAULT:-}" = wrong-version; then version=9.9.9; fi')
    original = original.replace(' printf \'{"jsonrpc"', ' if test "${THESEUS_NATIVE_FAULT:-}" = wrong-protocol; then result=$(printf "%s" "$result" | sed s/2026-07-28/2025-11-25/g); fi\n printf \'{"jsonrpc"')
    suffix = '\nif test "${THESEUS_NATIVE_FAULT:-}" = ignore-eof; then printf "THESEUS_DIRECT_EOF uid=%s\\n" "$(id -u)" >&2; while :; do sleep 1; done; fi\n'
    path.write_text(prefix + original + suffix)
    (root / 'TEST-ONLY-FAULTS.json').write_text(json.dumps({'original_target_sha256':original_sha256, 'target_fault_source_sha256':digest(path), 'note':'native target health unchanged; faults selected only by target direct-probe environment'}, indent=2))


def run(binary, smoke=False):
    clients, image = prerequisites()
    evidence, manifest = attempt(binary)
    identities = {kind:{'path':str(path),'sha256':digest(path),'size':path.stat().st_size,'expected_guest_version':VERSIONS[kind][1]} for kind,path in clients.items()}
    (evidence / 'native-inputs.json').write_text(json.dumps({'clients':identities,'image':image}, indent=2))
    roots = {}
    for era in ERAS:
        matrix = [(kind,scope,scenario,'standalone') for kind,scopes in [('claude',('user','project','local')),('codex',('global',))] for scope in scopes for scenario in ('clean','upgrade')]
        matrix.append(('claude','user','plugin_lifecycle','plugin'))
        if smoke:
            matrix = [row for row in matrix if row[2] in ('upgrade','plugin_lifecycle') and row[1] in ('user','global')]
        for kind,scope,scenario,delivery in matrix:
            root = prepare(evidence,clients,image,kind,scope,scenario,era,delivery)
            recipe = root / 'theseus.toml'
            name = root.name
            validated = command(evidence,name+'-validate',[binary,'--no-animation','validate','--recipe',recipe])
            assert validated.returncode == 0, validated.stderr
            code,path = run_cli(binary,evidence,name,root,recipe,image)
            report = verify(binary,evidence,name,path,'passed',image,clients[kind],era)
            assert code == 0, code
            roots[(kind,scope,scenario,era)] = root
    root = roots[('claude','user','upgrade',ERAS[0])]
    add_test_only_server_faults(root)
    for fault in ('duplicate','stale','settings-loss','wrong-tools','wrong-version','wrong-protocol','malformed','ignore-eof','blocked-stdin','timeout','cancel'):
        recipe = fault_recipe(root,fault)
        code,path = run_cli(binary,evidence,fault,root,recipe,image,cancel=fault=='cancel')
        expected = 'cancelled' if fault == 'cancel' else 'incomplete' if fault in ('ignore-eof','blocked-stdin','timeout') else 'failed'
        report = verify(binary,evidence,fault,path,expected,image,clients['claude'],ERAS[0])
        assert code == {'passed':0,'failed':1,'incomplete':2,'cancelled':130}[expected], code
        assert all(c['outcome']=='passed' for c in checks(report) if c['checkpoint']=='configured'), 'negative must follow successful native base'
        upgrade = next(c for c in commands(report) if c['phase']=='upgrade')
        assert upgrade['exit_status']==0, 'negative must follow successful native update'
        if fault in ('wrong-tools','wrong-version','wrong-protocol','malformed','ignore-eof','blocked-stdin','timeout','cancel'):
            target = [c for c in checks(report) if c['checkpoint']=='target_installed']
            assert all(c['outcome']=='passed' for c in target if not c.get('mcp_required')), 'native target registration/health must pass before direct fault'
            direct = next(c for c in target if c.get('mcp_required'))
            cmd = next(c for c in commands(report) if c['id']==direct['command_ids'][0])
            if expected in ('incomplete','cancelled'):
                assert commands(report)[-1]['id']==cmd['id'], 'no later action after unavailable probe'
                assert cmd['exit_status'] is None
                assert direct['mcp']['shutdown']=='forced_owned_removal'
            if fault=='wrong-tools':
                facts = wire_facts(direct['mcp']['response_lines'],ERAS[0])
                assert facts['tools']==['actually_wrong'] and direct['mcp']['issue']['kind']=='inventory'
            if fault in ('wrong-version','wrong-protocol'):
                response = strict_json(direct['mcp']['response_lines'][0])['result']
                if fault == 'wrong-version':
                    assert response['_meta']['io.modelcontextprotocol/serverInfo']['version'] == '9.9.9'
                else:
                    assert response['supportedVersions'] == ['2025-11-25']
                assert direct['mcp']['issue']['kind'] == 'protocol' and cmd['exit_status'] == 0
            if fault=='malformed':
                assert direct['mcp']['issue']['kind']=='protocol'
                assert (path.parent/cmd['stdout']['path']).read_bytes()==b'{invalid\n'
            if fault=='cancel':
                ready = strict_json((evidence/'cancel-cancel-ready.json').read_bytes())
                assert Path(ready['stderr_path'])==path.parent/cmd['stderr']['path']
                assert Path(ready['stdout_path'])==path.parent/cmd['stdout']['path']
                assert ready['stdout_bytes_at_ready'] == 0
                assert cmd['cancelled'] and not cmd['timed_out']
            if fault in ('timeout','blocked-stdin'):
                assert cmd['timed_out'] and not cmd['cancelled']
        else:
            failed = next(c for c in checks(report) if c['id']=='target_installed-'+('preservation' if fault=='settings-loss' else 'registration'))
            assert failed['outcome']=='failed'
    assert source_manifest(binary)==manifest, 'source or public binary changed during acceptance'
    assert all(digest(path)==identities[kind]['sha256'] for kind,path in clients.items())
    (evidence/'PASS').write_text(('6 exploratory' if smoke else '18 release-candidate') + ' actual-native matrix cases, eleven deliberate faults, independent received responses and strict owned absence passed.\n')


if __name__ == '__main__':
    run(Path(sys.argv[1]).resolve(), smoke='--smoke' in sys.argv[2:])
