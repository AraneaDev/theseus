"""Public preview editing acceptance. All fixture and terminal evidence is retained."""
import hashlib
import json
import os
import pathlib
import posixpath
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import tomllib

sys.dont_write_bytecode = True
from terminal_session import NativeSession, sentinel_environment
from docker_absence import require_container_absent

binary = str(pathlib.Path(sys.argv[1]).resolve())
case = sys.argv[2]
repo = pathlib.Path(__file__).resolve().parents[2]
evidence = pathlib.Path(tempfile.mkdtemp(prefix='theseus-preview-editing-task4-'))
print('Evidence:', evidence, flush=True)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def assert_private_acl(value):
    assert value['protected'] is True, value
    user = value['user']
    assert isinstance(user, str) and user.startswith('S-1-'), value
    expected = [{'sid': sid, 'type': 'Allow', 'rights': 0x1f01ff, 'inherited': False}
                for sid in (user, 'S-1-5-18')]
    assert sorted(value['rules'], key=lambda rule: rule['sid']) == sorted(
        expected, key=lambda rule: rule['sid']), value


def assert_private_file(path):
    if os.name != 'nt':
        assert path.stat().st_mode & 0o777 == 0o600
        return
    # Query this saved object's actual protected DACL, independently of the
    # writer. SIDs avoid localized account names; no path is interpolated as code.
    script = r"""
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$acl = Get-Acl -LiteralPath $env:THESEUS_PRIVATE_FILE
$rules = @($acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
if ($rules.Count -gt 32) { throw 'private file ACE bound exceeded' }
$value = @{
    user = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    protected = $acl.AreAccessRulesProtected
    rules = @($rules | ForEach-Object { @{
        sid = $_.IdentityReference.Value
        type = $_.AccessControlType.ToString()
        rights = [long]$_.FileSystemRights
        inherited = $_.IsInherited
    } })
}
$json = ConvertTo-Json -InputObject $value -Depth 4 -Compress
if ($json.Length -gt 16384) { throw 'private file ACL receipt bound exceeded' }
[Console]::WriteLine($json)
"""
    result = subprocess.run(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', script],
                            env={**os.environ, 'THESEUS_PRIVATE_FILE': str(path)},
                            capture_output=True, timeout=15)
    receipt = evidence / f'{path.parent.name}-{path.name}.acl'
    receipt.with_suffix('.acl.stdout').write_bytes(result.stdout)
    receipt.with_suffix('.acl.stderr').write_bytes(result.stderr)
    assert len(result.stdout) <= 16384 and len(result.stderr) <= 16384, 'ACL query output exceeded bound'
    assert result.returncode == 0, result.stderr
    assert_private_acl(json.loads(result.stdout))


def manifest(root):
    return {p.relative_to(root).as_posix(): digest(p) for p in root.rglob('*') if p.is_file()}


def expected_suite_manifest(saved):
    """Build schema-1 field order from saved TOML, independent of the artifact."""
    source = tomllib.loads(saved.read_text())
    return {
        'schema_version': source['schema_version'],
        'name': source['name'],
        'baseline': source['baseline'],
        'profiles': source['profiles'],
        'scenarios': source['scenarios'],
        'variants': [{'id': v['id'], 'recipe': v['recipe']} for v in source['variants']],
        'scopes': [{'id': scope['id'], 'profile': scope['profile'],
                    'path': posixpath.normpath(scope['path'])} for scope in source['scopes']],
        'stages': source['stages'],
    }


def assert_suite_provenance(value, saved, artifact_path):
    expected = expected_suite_manifest(saved)
    assert value['manifest'] == expected, 'saved suite manifest differs from artifact'
    canonical = json.dumps(expected, ensure_ascii=False, separators=(',', ':')).encode()
    assert value['manifest_digest'] == hashlib.sha256(canonical).hexdigest(), 'manifest digest differs'
    declared = {v['id']: digest(saved.parent / v['recipe']) for v in expected['variants']}
    assert value['recipe_digests'] == declared, 'variant recipe digests differ'
    ids = [v['id'] for v in expected['variants']]
    assert [s['variant_id'] for s in value['report']['sources']] == ids
    assert [s['variant_id'] for s in value['sources']] == ids
    for source, reference in zip(value['report']['sources'], value['sources']):
        variant = source['variant_id']
        assert source['outcome'] == 'passed' and source['error'] is None, source
        assert source['recipe_digest'] == declared[variant], source
        assert reference['error'] is None and reference['report_digest'] is not None, reference
        assert source['report_path'] == reference['path'] + '/report.json', source
        source_path = artifact_path.parent / source['report_path']
        assert source_path.is_file(), source_path
        assert reference['report_digest'] == digest(source_path), reference
        actual = json.loads(source_path.read_text())
        assert actual['outcome'] == 'passed', actual
        assert actual['recipe_digest'] == declared[variant], actual
        assert pathlib.Path(actual['recipe_path']).samefile(saved.parent / next(
            v['recipe'] for v in expected['variants'] if v['id'] == variant)), actual


def fixture(kind, name):
    root = evidence / name
    source = repo / ('tests/fixtures/lifecycle' if kind == 'recipe' else 'examples/comparisons')
    shutil.copytree(source, root)
    return root, root / ('recipe.toml' if kind == 'recipe' else 'methods.toml')


class Session:
    def __init__(self, root, name, args, sentinel=True, animation=False):
        self.root, self.name = root, name
        self.sentinel = root / (name + '.docker-called')
        env = sentinel_environment(root / (name + '-bin'), self.sentinel) if sentinel else dict(os.environ)
        self.native = NativeSession(binary, ['--no-animation', *args] if not animation else args,
                                    root, env, 42, 140)
        self.proc = self.native
        self.screen = self.native.output.screen
        self.raw = self.native.output.data
        self.before = self.native.modes['before']

    def pump(self):
        self.native.pump()

    def text(self):
        return self.screen.text().decode('utf8', 'replace')

    def until(self, token, seconds=8):
        deadline = time.monotonic() + seconds
        while token not in self.text() and self.proc.poll() is None and time.monotonic() < deadline:
            self.pump()
        assert token in self.text(), (self.name, token, self.proc.poll(), self.text())

    def key(self, data):
        self.native.key(data)

    def snap(self, label):
        (evidence / f'{self.name}-{label}.screen').write_bytes(self.screen.text())

    def finish(self, code=0):
        assert self.proc.wait(15) == code, (self.name, self.proc.returncode, self.text())
        self.native.assert_restored()
        assert b'\x1b[?1049h' in self.raw and b'\x1b[?1049l' in self.raw, 'alternate screen not restored'
        assert b'\x1b[?2004h' in self.raw and b'\x1b[?2004l' in self.raw, 'bracketed paste not restored'
        assert self.raw.rfind(b'\x1b[?1049l') > self.raw.rfind(b'\x1b[?1049h'), 'alternate screen re-entered after leave'
        assert self.raw.rfind(b'\x1b[?2004l') > self.raw.rfind(b'\x1b[?2004h'), 'bracketed paste re-enabled after disable'
        assert not self.sentinel.exists(), 'preview invoked Docker'

    def close(self):
        try:
            self.native.close()
        finally:
            (evidence / (self.name + '.raw')).write_bytes(self.raw)
            (evidence / (self.name + '.terminal.json')).write_text(json.dumps({
                'exit': self.proc.returncode, 'modes': self.native.modes}, default=list), encoding='utf-8')


def args(kind, source):
    if kind == 'recipe':
        return ['run', '--recipe', str(source), '--profile', 'user', '--scenario', 'clean']
    return ['compare', '--suite', str(source)]


def name_in_prompt(s, old, new):
    s.key(b'\x1b[H' + b'\x1b[3~' * len(old))
    s.key(new.encode() + b'\r')


def choose_name(s, source, new):
    s.key(b'\x13')
    s.until('SAVE COPY')
    name_in_prompt(s, source.stem + '.edited.toml', new)


def preview(kind):
    root, source = fixture(kind, kind + '-preview')
    collision = root / 'occupied.toml'
    collision.write_bytes(b'original collision\n')
    link = root / 'linked.toml'
    link.symlink_to(collision)
    before = manifest(root)
    (evidence / (kind + '-before.json')).write_text(json.dumps(before, indent=2))
    s = Session(root, kind + '-preview', args(kind, source))
    try:
        s.until('Preview')
        assert not (root / ('.theseus-runs' if kind == 'recipe' else '.theseus-comparisons')).exists()
        s.key(b'\x1b[200~r\nq\x1b[201~')
        s.pump()
        assert not (root / ('.theseus-runs' if kind == 'recipe' else '.theseus-comparisons')).exists()
        # Exercise every delimiter boundary through the real terminal transport.
        # The terminal reader must keep payload commands inert between writes.
        hostile = b'\x1b[200~r\nq\x1b[201~'
        for split in ([*range(1, 6), *range(len(hostile) - 5, len(hostile))] if os.name == 'nt' else []):
            s.key(hostile[:split])
            s.pump()
            assert s.proc.poll() is None, 'partial paste quit preview'
            assert not (root / ('.theseus-runs' if kind == 'recipe' else '.theseus-comparisons')).exists()
            s.key(hostile[split:])
            s.pump()
            assert s.proc.poll() is None, 'paste payload quit preview'
            assert not (root / ('.theseus-runs' if kind == 'recipe' else '.theseus-comparisons')).exists()
        s.key(b'e')
        s.until('EDIT unchanged')
        s.key(b'\x1b[1;5H')  # Ctrl-Home
        s.key(b'qrsd')  # ordinary controls are literal editor text
        s.until('EDIT UNSAVED')
        s.key(b'\x14')  # Ctrl-T: invalid TOML, no adoption
        s.until('Invalid draft')
        s.snap('invalid')
        choose_name(s, source, 'bad-copy.toml')
        s.until('Save Copy failed')
        assert not (root / 'bad-copy.toml').exists(), 'invalid draft persisted'
        s.key(b'\x1b')  # return to editor from the failed name prompt
        s.until('EDIT UNSAVED')
        s.key(b'\x1b')
        s.until('UNSAVED draft')
        s.key(b'rps')
        s.pump()
        assert not (root / ('.theseus-runs' if kind == 'recipe' else '.theseus-comparisons')).exists()
        s.key(b'd')
        s.until('Discard unsaved draft?')
        s.key(b'\x1b')
        s.until('UNSAVED draft')
        s.key(b'd')
        s.until('Discard unsaved draft?')
        s.key(b'\r')
        s.until('Preview')
        s.key(b'e')
        s.until('EDIT unchanged')
        s.key(b'\x1b[1;5H')
        comment = '# q r ñ 😀\n'.encode()
        s.key(b'\x1b[200~' + comment + b'\x1b[201~')
        s.until('EDIT UNSAVED')
        s.snap('valid-draft')
        s.key(b'\x14')
        s.until('Draft valid')
        choose_name(s, source, 'occupied.toml')
        s.until('Save Copy failed')
        assert collision.read_bytes() == b'original collision\n'
        s.snap('collision')
        name_in_prompt(s, 'occupied.toml', 'linked.toml')
        s.until('Save Copy failed')
        assert link.is_symlink() and collision.read_bytes() == b'original collision\n'
        s.snap('symlink')
        s.key(b'\x1b')
        s.until('EDIT UNSAVED')
        chosen = 'verified-copy.toml'
        choose_name(s, source, chosen)
        s.until('Preview')
        saved = root / chosen
        assert saved.read_bytes() == comment + source.read_bytes()
        assert_private_file(saved)
        assert saved.resolve().parent == source.resolve().parent
        assert not (root / ('.theseus-runs' if kind == 'recipe' else '.theseus-comparisons')).exists()
        s.snap('saved-preview')
        s.key(b'q')
        s.finish()
    finally:
        s.close()
    after = manifest(root)
    (evidence / (kind + '-after.json')).write_text(json.dumps(after, indent=2))
    assert all(after.get(k) == v for k, v in before.items()), 'original fixture changed'
    assert set(after) - set(before) == {
        'verified-copy.toml', f'{s.name}-bin/' + ('docker.exe' if os.name == 'nt' else 'docker')
    }, 'unexpected file created during preview editing'
    assert after['verified-copy.toml'] == digest(root / 'verified-copy.toml')


def dirty_quit():
    root, source = fixture('recipe', 'dirty-quit')
    before = digest(source)
    s = Session(root, 'dirty-quit', args('recipe', source))
    try:
        s.until('Preview')
        s.key(b'e')
        s.until('EDIT unchanged')
        s.key(b'x\x1b')
        s.until('UNSAVED draft')
        s.key(b'q')
        s.until('Discard unsaved draft and quit?')
        s.key(b'\x1b')
        s.until('UNSAVED draft')
        s.key(b'q')
        s.until('Discard unsaved draft and quit?')
        s.key(b'\r')
        s.finish()
        assert digest(source) == before
        assert not (root / '.theseus-runs').exists()
    finally:
        s.close()


def offline_inert():
    root = evidence / 'offline'
    root.mkdir()
    source = root / 'report.json'
    shutil.copyfile(repo / 'tests/fixtures/reports/schema1-report.json', source)
    before = digest(source)
    s = Session(root, 'offline', ['report', str(source)])
    try:
        s.until('Offline report')
        s.key(b'e')
        s.pump()
        assert 'EDIT unchanged' not in s.text(), 'offline report opened editor'
        s.key(b'\x1b[200~r\nq\x1b[201~')
        s.pump()
        assert s.proc.poll() is None, 'paste outside editor ran commands'
        s.snap('inert')
        s.key(b'q')
        s.finish()
        assert digest(source) == before
    finally:
        s.close()


def signals():
    for name, action in (('CtrlC', b'\x03'), ('SIGINT', signal.SIGINT), ('SIGTERM', signal.SIGTERM)):
        root, source = fixture('recipe', 'signal-' + name)
        initial = digest(source)
        s = Session(root, name, args('recipe', source))
        try:
            s.until('Preview')
            s.key(b'e')
            s.until('EDIT unchanged')
            s.key(b'x')
            s.until('EDIT UNSAVED')
            if isinstance(action, bytes):
                s.key(action)
            else:
                s.proc.send_signal(action)
            s.finish()
            assert digest(source) == initial
        finally:
            s.close()


def docker():
    out = subprocess.run(['docker', 'info'], capture_output=True)
    assert out.returncode == 0, out.stderr
    out = subprocess.run(['docker', 'image', 'inspect', 'ubuntu:24.04'], capture_output=True)
    assert out.returncode == 0, out.stderr
    for kind in ('recipe', 'suite'):
        root, source = fixture(kind, kind + '-docker')
        s = Session(root, kind + '-docker', args(kind, source), sentinel=False)
        try:
            s.until('Preview')
            s.key(b'e')
            s.until('EDIT unchanged')
            s.key(b'\x1b[1;5H')
            s.key(b'\x1b[200~' + '# saved source ñ\n'.encode() + b'\x1b[201~')
            s.until('EDIT UNSAVED')
            s.key(b'\x13')
            s.until('SAVE COPY')
            name_in_prompt(s, source.stem + '.edited.toml', 'saved.toml')
            s.until('Preview')
            saved = root / 'saved.toml'
            assert saved.exists()
            assert not list(root.glob('.theseus-runs/*/report.json'))
            assert not list(root.glob('.theseus-comparisons/*/comparison.json'))
            s.key(b'r')
            pattern = '.theseus-runs/*/report.json' if kind == 'recipe' else '.theseus-comparisons/*/comparison.json'
            deadline = time.monotonic() + 120
            while not list(root.glob(pattern)) and time.monotonic() < deadline:
                s.pump()
            reports = list(root.glob(pattern))
            assert len(reports) == 1, (kind, s.text())
            report = reports[0]
            def complete():
                state = json.loads(report.read_text())
                return state.get('finished_at') is not None if kind == 'recipe' else state.get('finished', False)
            while not complete() and time.monotonic() < deadline:
                s.pump()
            assert complete(), (kind, s.text())
            value = json.loads(report.read_text())
            (evidence / (kind + '-actual-report.json')).write_text(json.dumps(value, indent=2))
            if kind == 'recipe':
                assert value['outcome'] == 'passed', value
                assert pathlib.Path(value['recipe_path']).samefile(saved)
                assert value['recipe_digest'] == digest(saved)
            else:
                assert value['finished'] and value['report']['outcome'] == 'passed', value
                assert_suite_provenance(value, saved, report)
                (evidence / 'suite-saved-source.json').write_text(json.dumps({
                    'path': str(saved.resolve()), 'raw_sha256': digest(saved)}, indent=2))
            s.key(b'q')
            s.finish()
        finally:
            prior_error = sys.exc_info()[1]
            s.close()
            # Even a failed assertion leaves only owned evidence for cleanup.
            for index, candidate in enumerate(list(root.glob('.theseus-runs/*/report.json')) +
                                              list(root.glob('.theseus-comparisons/*/comparison.json'))):
                clean = subprocess.run([binary, 'cleanup', str(candidate.parent)], capture_output=True)
                (evidence / f'{kind}-cleanup-{index}.stdout').write_bytes(clean.stdout)
                (evidence / f'{kind}-cleanup-{index}.stderr').write_bytes(clean.stderr)
                (evidence / f'{kind}-cleanup-{index}.status').write_text(str(clean.returncode))
                if prior_error is None:
                    assert clean.returncode == 0, clean.stderr
        for journal in root.glob('.theseus-runs/*/journal.json'):
            resources = json.loads(journal.read_text())['resources']
            assert all(resource['removed'] for resource in resources), resources
            for resource in resources:
                require_container_absent(resource['container_id'], subprocess.run(
                    ['docker', 'inspect', resource['container_id']], capture_output=True, text=True))
        for journal in root.glob('.theseus-comparisons/*/sources/*/*/journal.json'):
            resources = json.loads(journal.read_text())['resources']
            assert all(resource['removed'] for resource in resources), resources
            for resource in resources:
                require_container_absent(resource['container_id'], subprocess.run(
                    ['docker', 'inspect', resource['container_id']], capture_output=True, text=True))


if case == 'preview':
    preview('recipe')
    preview('suite')
    dirty_quit()
    offline_inert()
    signals()
elif case == 'docker':
    docker()
else:
    raise AssertionError(case)
print('PASS', case, flush=True)
