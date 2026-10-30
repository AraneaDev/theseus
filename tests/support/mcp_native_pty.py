"""Public native live/offline/cancel PTYs. Current text cells, not human/PNG review."""
import fcntl
import json
import os
from pathlib import Path
import signal
import struct
import termios
import sys
from mcp_native import (ERAS, add_test_only_server_faults, attempt, checks, commands, digest,
    direct_ready, fault_recipe, prepare, prerequisites, run_cli, source_manifest, stages, verify)
from plugin_native_pty import Session, latest
from terminal_screen import Screen


class McpSession(Session):
    def key(self, value):
        self.steps.append({'key_hex':value.hex()})
        super().key(value)

    def snap(self, label):
        self.steps.append({'snapshot':label,'columns':self.screen.columns,'rows':self.screen.rows})
        (self.evidence / (self.name+'-'+label+'.screen')).write_bytes(self.screen.text())

    def detail(self):
        lines = self.text().splitlines()
        for row,line in enumerate(lines):
            if 'Detail — record' in line:
                at = line.index('Detail — record')
                left, right = line.rfind('┌',0,at), line.find('┐',at)
                assert left >= 0 and right > left, line
                body = []
                for following in lines[row+1:]:
                    if following[left:left+1] == '└':
                        break
                    body.append(following[left+1:right])
                return '\n'.join(body)
        return ''

    def detail_contains(self, text):
        return ' '.join(text.split()) in ' '.join(self.detail().split())

    def expect_detail(self, text):
        self.until(lambda:self.detail_contains(text), 'selected detail: '+text)

    def seek_detail(self, text):
        for _ in range(12):
            for _ in range(3): self.pump()
            if self.detail_contains(text): return
            self.key(b'\x1b[6~')
            for _ in range(3): self.pump()
            if self.detail_contains(text): return
            self.key(b']')
        self.expect_detail(text)

    def record(self, query, detail):
        self.filtered(query)
        self.key(b'\x1b[F')
        self.expect_detail(detail)

    def stage(self, index):
        # Separate bare Escape events so they cannot be decoded as Alt-Escape.
        for key in (b'\x1b', b'\x1b', b'\t', b'\x1b[H', b'j'*index, b'\t'):
            if key:
                self.key(key)
                for _ in range(3): self.pump()
        self.until(lambda:'Records' in self.text(), 'stage/evidence focus')

    def resize(self, rows, columns):
        fcntl.ioctl(self.slave,termios.TIOCSWINSZ,struct.pack('HHHH',rows,columns,0,0))
        self.screen = Screen(rows,columns)
        self.steps.append({'resize':{'rows':rows,'columns':columns}})
        os.kill(self.proc.pid,signal.SIGWINCH)
        self.until(lambda:'Theseus' in self.text() and 'Detail — record' in self.text(), 'redraw at actual new geometry')


def browse(session, report):
    contract = report['scenarios'][0]['mcp']
    plugin = contract['delivery']=='plugin'
    cp = 'base_installed' if plugin else 'configured'
    session.stage(next(i for i,s in enumerate(stages(report)) if s['name']==cp))
    session.record('MCP '+contract['adapter'], 'scope: '+contract['scope'])
    session.expect_detail(contract['client_version'])
    session.seek_detail('independent typed stdio discovery')
    if contract['client_claim']=='configuration_only':
        session.seek_detail('native activation not exposed/tested')
        assert 'Connected' not in session.detail()
    session.snap('client-scope-claim-boundary')
    artifact = next(a for a in report['artifacts'] if a['name']=='target')
    client = next(f for f in artifact['files'] if f['path']=='client/claude')
    session.record('Artifact target','Artifact target')
    session.seek_detail('client/claude')
    session.seek_detail(client['sha256'])
    session.snap('supplied-client-file-digest')
    session.record(cp+'-registration', 'JSON proof:')
    session.seek_detail('Observations:')
    session.snap('registration-observations')
    session.record(cp+'-probe', 'MCP protocol: '+ERAS[0])
    session.expect_detail('Observed server:')
    session.expect_detail('hello_v1')
    session.expect_detail('Shutdown: Graceful')
    assert '"jsonrpc"' not in session.detail()
    session.snap('base-received-identity-inventory-shutdown')
    session.resize(36,120)
    session.expect_detail('hello_v1')
    session.snap('wide-probe-current-cells')
    session.resize(50,80)
    session.expect_detail('hello_v1')
    session.snap('narrow-probe-current-cells')
    session.filtered('')
    session.key(b'\x1b[C'*4) # Stages -> Logs
    cmd = next(c for c in commands(report) if c['phase']=='check:'+cp+'-probe')
    session.record(cmd['id']+' stdout','"jsonrpc"')
    session.until(lambda:'raw bytes' in session.text(),'bounded log range')
    session.snap('bounded-response-log')
    session.key(b'm')
    session.expect_detail('truncated=false')
    session.until(lambda:'log metadata' in session.text(),'log metadata mode')
    session.snap('bounded-response-log-metadata')
    session.key(b'm')
    session.filtered('')
    session.key(b'\x1b[C') # Logs -> Stages
    session.stage(next(i for i,s in enumerate(stages(report)) if s['name']=='target_installed'))
    session.record('target_installed-probe','MCP protocol: '+ERAS[0])
    session.expect_detail('hello_v2')
    session.expect_detail('2.0.0')
    session.snap('updated-received-inventory')


def run(binary):
    binary = str(binary)
    clients,image = prerequisites()
    evidence,manifest = attempt(binary)
    (evidence/'executed-pty-harness.py').write_bytes(Path(__file__).read_bytes())
    root = prepare(evidence,clients,image,'claude','user','plugin_lifecycle',ERAS[0],'plugin')
    recipe = root/'theseus.toml'
    s = McpSession(binary,evidence,'live',['run','--recipe',recipe,'--profile','user','--scenario','plugin-lifecycle'])
    try:
        s.expect('Preview')
        assert not (root/'.theseus-runs').exists()
        s.snap('preview-explicit-start')
        s.key(b'r')
        # Saved finished_at precedes the public engine-finished event. Focus only
        # after the actual public terminal shows its final Finished status.
        s.until(lambda:'Finished |' in s.text(),'public engine finished screen',180)
        path = latest(root,recipe)
        assert path is not None
        report = verify(binary,evidence,'pty-live',path,'passed',image,clients['claude'],ERAS[0])
        browse(s,report)
        s.key(b'q')
        s.finish(0)
    finally: s.close()
    before = {str(p.relative_to(path.parent)):digest(p) for p in path.parent.rglob('*') if p.is_file()}
    s = McpSession(binary,evidence,'offline',['report',path.parent],offline=True)
    try:
        s.expect('Offline report')
        browse(s,report)
        s.key(b'rq')
        s.finish(0)
    finally: s.close()
    assert before=={str(p.relative_to(path.parent)):digest(p) for p in path.parent.rglob('*') if p.is_file()}
    codex = prepare(evidence,clients,image,'codex','global','upgrade',ERAS[0])
    code,codex_path = run_cli(binary,evidence,'codex-offline-input',codex,codex/'theseus.toml',image)
    codex_report = verify(binary,evidence,'codex-offline-input',codex_path,'passed',image,clients['codex'],ERAS[0])
    assert code==0
    before = digest(codex_path)
    s = McpSession(binary,evidence,'codex-offline',['report',codex_path.parent],offline=True)
    try:
        s.expect('Offline report')
        browse(s,codex_report)
        s.key(b'rq')
        s.finish(0)
    finally: s.close()
    assert digest(codex_path)==before
    root = prepare(evidence,clients,image,'claude','user','upgrade',ERAS[0])
    add_test_only_server_faults(root)
    recipe = fault_recipe(root,'cancel')
    s = McpSession(binary,evidence,'cancel',['run','--recipe',recipe,'--profile','user','--scenario','upgrade'])
    try:
        s.expect('Preview')
        s.key(b'r')
        ready = None
        def waiting():
            nonlocal ready
            for candidate in (root/'.theseus-runs').glob('*/report.json'):
                ready = direct_ready(candidate.parent/'logs','acceptance-target-v2',ERAS[0])
                if ready: return True
            return False
        s.until(waiting,'specific v2 direct probe request entered hold',180)
        (evidence/'pty-cancel-ready.json').write_text(json.dumps(ready,indent=2))
        s.snap('direct-probe-active-before-cancel')
        s.key(b'c')
        s.until(lambda:'Finished |' in s.text(),'public engine finished screen',180)
        path = latest(root,recipe)
        report = verify(binary,evidence,'pty-cancel',path,'cancelled',image,clients['claude'],ERAS[0])
        direct = next(c for c in commands(report) if c['phase']=='check:target_installed-probe')
        assert direct['cancelled'] and direct['exit_status'] is None
        assert commands(report)[-1]['id']==direct['id']
        assert Path(ready['stderr_path'])==path.parent/direct['stderr']['path']
        assert Path(ready['stdout_path'])==path.parent/direct['stdout']['path']
        assert ready['stdout_bytes_at_ready']==0
        assert all(c['outcome']=='passed' for c in checks(report) if c['checkpoint']=='configured')
        s.stage(next(i for i,stage in enumerate(stages(report)) if stage['name']=='target_installed'))
        s.record('target_installed-probe','MCP protocol: '+ERAS[0])
        s.expect_detail('ForcedOwnedRemoval')
        s.snap('cancelled-probe-retained-disposition')
        s.key(b'q')
        s.finish(130)
    finally: s.close()
    assert source_manifest(binary)==manifest
    (evidence/'PASS').write_text('Real native live/offline/cancel public PTYs: current detail cells, actual resize, bounded raw logs, config-only boundary, sentinel, unchanged saved evidence, termios restored. Text oracle, no physical/human PNG review.\n')


if __name__=='__main__':
    run(Path(sys.argv[1]).resolve())
