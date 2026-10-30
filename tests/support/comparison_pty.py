"""Public CLI/Docker/PTY acceptance. Retains unique evidence; removes owned tags only."""
import fcntl, json, os, pathlib, pty, select, shutil, signal, struct, subprocess, sys, tempfile, termios, time
from terminal_screen import Screen
from docker_absence import require_container_absent
from comparison_cleanup import fallback_cleanup
binary = str(pathlib.Path(sys.argv[1]).resolve())
case = sys.argv[2]
repo = pathlib.Path(__file__).resolve().parents[2]
evidence = pathlib.Path(tempfile.mkdtemp(prefix=os.environ.get('THESEUS_COMPARISON_EVIDENCE_PREFIX', 'theseus-comparison-task4-') + case + '-'))
print('Evidence:', evidence, flush=True)
owned_tags = []
roots = []

def command(args, name, expected=0, **kw):
    out = subprocess.run(args, capture_output=True, **kw)
    (evidence/(name+'.stdout')).write_bytes(out.stdout)
    (evidence/(name+'.stderr')).write_bytes(out.stderr)
    (evidence/(name+'.status')).write_text(str(out.returncode))
    assert out.returncode == expected, (args, out.returncode, out.stdout[-2000:], out.stderr[-2000:])
    return out

def fixture(name):
    root = evidence/name
    shutil.copytree(repo/'examples/comparisons', root)
    roots.append(root)
    return root

def suite_root(root):
    paths = list((root/'.theseus-comparisons').glob('*/comparison.json'))
    assert len(paths) == 1, paths
    return paths[0].parent

def offline(root, name):
    saved = suite_root(root)
    # A valid artifact must be readable with no Docker in PATH, including failed/cancelled outcomes.
    out = command([binary,'comparison',str(saved),'--json'], name+'-offline', env={**os.environ,'PATH':'/no-docker'})
    result = json.loads(out.stdout)
    for source in result['sources']:
        if source['report_path']:
            report = json.loads((saved/source['report_path']).read_text())
            for scenario in report['scenarios']:
                assert scenario['teardown']['status'] == 'complete', scenario
                if scenario['container_id']: require_container_absent(scenario['container_id'], subprocess.run(['docker','inspect',scenario['container_id']],capture_output=True,text=True))
    command([binary,'cleanup',str(saved)], name+'-cleanup')
    return result

def run(root, manifest='methods', expected=0):
    command([binary,'compare','--suite',str(root/(manifest+'.toml')),'--headless'], root.name+'-run',expected)
    return offline(root,root.name)

def slow(root):
    p=root/'copy.toml'
    p.write_text(p.read_text().replace('script = "mkdir', 'script = "touch /tmp/comparison-ready; sleep 60; mkdir'))

def ready(root):
    for path in root.glob('.theseus-comparisons/*/sources/copy/*/journal.json'):
        journal=json.loads(path.read_text())
        for resource in journal['resources']:
            out=subprocess.run(['docker','exec',resource['container_id'],'test','-f','/tmp/comparison-ready'],capture_output=True)
            if out.returncode==0: return True
    return False

def wait(predicate, pump=lambda:time.sleep(.05), seconds=30):
    deadline=time.monotonic()+seconds
    while time.monotonic()<deadline:
        if predicate(): return
        pump()
    assert predicate(), 'acceptance condition did not become true'

class Session:
    def __init__(self, root, name, args, animation=False, read_only=False):
        self.root,self.name=root,name
        self.master,self.slave=pty.openpty()
        self.before=termios.tcgetattr(self.slave)
        fcntl.ioctl(self.slave,termios.TIOCSWINSZ,struct.pack('HHHH',54,180,0,0))
        self.screen=Screen(54,180); self.data=bytearray();self.captions=set()
        environment={**os.environ,'TERM':'xterm-256color'}
        self.docker_sentinel=root/(name+'-docker-called')
        if read_only:
            bindir=root/(name+'-bin');bindir.mkdir()
            docker=bindir/'docker';docker.write_text('#!/bin/sh\nprintf called > "$THESEUS_DOCKER_SENTINEL"\nexit 111\n');docker.chmod(0o755)
            environment.update(PATH=str(bindir)+os.pathsep+os.environ['PATH'],THESEUS_DOCKER_SENTINEL=str(self.docker_sentinel))
        self.proc=subprocess.Popen([binary,*args,*([] if animation else ['--no-animation'])],cwd=root,stdin=self.slave,stdout=self.slave,stderr=self.slave,env=environment)
    def pump(self):
        if select.select([self.master],[],[],.05)[0]:
            chunk=os.read(self.master,65536); self.data.extend(chunk); self.screen.feed(chunk)
            for caption in ['Entering the labyrinth','Following the thread home']:
                if self.has(caption):self.captions.add(caption)
    def has(self,text): return text.encode() in self.screen.text()
    def until(self,predicate): wait(predicate,self.pump)
    def key(self,text): os.write(self.master,text)
    def snapshot(self,name): (evidence/(self.name+'-'+name+'.screen')).write_bytes(self.screen.text())
    def finish(self,code):
        self.until(lambda:self.proc.poll() is not None)
        while select.select([self.master],[],[],.05)[0]: self.pump()
        assert self.proc.returncode==code,(self.proc.returncode,self.screen.text())
        assert not self.docker_sentinel.exists(),'read-only view invoked Docker'
        assert termios.tcgetattr(self.slave)==self.before,'termios not restored'
        assert b'\x1b[?1049h' in self.data and b'\x1b[?1049l' in self.data
    def close(self):
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:self.proc.wait(timeout=8)
            except subprocess.TimeoutExpired:self.proc.kill();self.proc.wait()
        (evidence/(self.name+'.raw')).write_bytes(self.data)
        (evidence/(self.name+'.terminal.json')).write_text(json.dumps({'exit':self.proc.returncode,'before':self.before,'after':termios.tcgetattr(self.slave)}, default=lambda value: list(value)))
        os.close(self.master);os.close(self.slave)

def evidence_navigation(s):
    # Exercise the public cell/list/detail focus boundary, rather than relying
    # on the former flattened pane keeping every source/command visible.
    s.until(lambda:s.has('Variant: archive | user/clean | Stage: configured'))
    s.until(lambda:s.has('Scope user-binary: /home/theseus/.local/bin/comparison-cli'))
    s.key(b'\t\x1b[F')
    s.until(lambda:s.has('Complete evidence: theseus comparison'))
    s.until(lambda:s.has('--json'))
    s.snapshot('json-detail')
    s.key(b'\x1b[H')
    s.until(lambda:s.has('Detail — record 1 /'))
    s.until(lambda:s.has('archive vs copy | user | clean | configured'))
    s.snapshot('cell-detail')
    s.key(b'\t')  # restore cell focus for the existing j/k assertions

def pty_cases():
    root=fixture('preview')
    s=Session(root,'preview',['compare','--suite','methods.toml'],True,True)
    try:
        s.until(lambda:s.has('T H E S E U S'))
        s.key(b'rps') # ignored during intro; no queued run/selection mutation
        for _ in range(4):s.pump()
        assert not (root/'.theseus-comparisons').exists()
        s.key(b' ');s.until(lambda:s.has('Preview'))
        s.key(b'ps');s.key(b'\x1b[6~');s.key(b'\x1b[5~')
        s.snapshot('preview'); assert not (root/'.theseus-comparisons').exists()
        s.key(b'q');s.until(lambda:s.has('Following the thread home'));s.key(b' ');s.finish(0)
    finally:s.close()
    root=fixture('live')
    # Leave enough actual lifecycle work to navigate the live view deterministically.
    p=root/'copy.toml';p.write_text(p.read_text().replace('script = "comparison-cli configure"','script = "sleep 2; comparison-cli configure"'))
    s=Session(root,'live',['compare','--suite','methods.toml'])
    try:
        s.until(lambda:s.has('Preview'));assert not (root/'.theseus-comparisons').exists()
        s.key(b'r');s.until(lambda:s.has('Comparison rows'))
        s.key(b'jj');s.until(lambda:s.has('archive vs copy | user | clean | configured'));s.snapshot('selected')
        wait(lambda:bool(list(root.glob('.theseus-comparisons/*/comparison.json'))) and json.loads(next(root.glob('.theseus-comparisons/*/comparison.json')).read_text())['finished'],s.pump,90)
        s.until(lambda:s.has('Equivalent'));evidence_navigation(s);s.key(b'k');s.key(b'\x1b[6~');s.key(b'\x1b[5~');s.snapshot('complete');s.key(b'q');s.finish(0)
    finally:s.close()
    report=offline(root,'live');assert report['outcome']=='passed'
    saved=suite_root(root);before=(saved/'comparison.json').read_bytes()
    s=Session(root,'offline',['comparison',str(saved)],read_only=True)
    try:
        s.until(lambda:s.has('Equivalent'));s.key(b'jj');s.until(lambda:s.has('archive vs copy | user | clean | configured'));evidence_navigation(s);s.key(b'\x1b[6~');s.key(b'\x1b[5~');s.snapshot('selected');s.key(b'r');s.key(b'q');s.finish(0)
        assert (saved/'comparison.json').read_bytes()==before,'offline r mutated suite'
    finally:s.close()
    root=fixture('active-cancel');slow(root)
    s=Session(root,'active-cancel',['compare','--suite','methods.toml'],True)
    try:
        s.until(lambda:s.has('T H E S E U S'));s.key(b' ');s.until(lambda:s.has('Preview'));s.key(b'r')
        wait(lambda:ready(root),s.pump)
        started=time.monotonic();s.key(b'q');s.finish(130)
        assert time.monotonic()-started<5,'active quit exceeded cleanup deadline'
        assert 'Following the thread home' not in s.captions,'active quit played outro'
    finally:s.close()
    result=offline(root,'active-cancel');assert result['outcome']=='cancelled';assert result['sources'][1]['report_path'] is None

try:
    command(['docker','info'],'docker-info')
    command(['docker','image','inspect','ubuntu:24.04'],'base-image')
    if case=='pty':pty_cases()
    elif case=='cancel':
        for profile in ['user','system']:
            root=fixture('cancel-'+profile);slow(root)
            p=root/'methods.toml';text=p.read_text().replace('profiles = ["user", "system"]',f'profiles = ["{profile}"]')
            # Exclude the other profile's scopes without altering the recipes.
            blocks=text.split('\n[[scopes]]');text=blocks[0]+''.join('\n[[scopes]]'+b for b in blocks[1:] if f'profile = "{profile}"' in b);p.write_text(text)
            proc=subprocess.Popen([binary,'compare','--suite',str(p),'--headless'],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
            try:
                wait(lambda:ready(root));proc.send_signal(signal.SIGINT);stdout,stderr=proc.communicate(timeout=10)
                (evidence/(profile+'.stdout')).write_bytes(stdout);(evidence/(profile+'.stderr')).write_bytes(stderr)
                assert proc.returncode==130,(proc.returncode,stderr)
            finally:
                if proc.poll() is None:proc.kill();proc.wait()
            result=offline(root,profile);assert result['outcome']=='cancelled';assert result['sources'][1]['report_path'] is None
    else:
        root=fixture(case)
        manifest='methods';expected=0
        if case=='images':
            tag='theseus-comparison-task4:'+str(os.getpid())+'-'+str(time.time_ns());owned_tags.append(tag)
            command(['docker','build','--pull=false','--network=none','-f',str(root/'Dockerfile.marker'),'-t',tag,str(root)],'build-marker')
            base=json.loads(command(['docker','image','inspect','ubuntu:24.04'],'inspect-base').stdout)[0]['Id']
            derived=json.loads(command(['docker','image','inspect',tag],'inspect-marker').stdout)[0]['Id'];assert base!=derived
            p=root/'image-marker.toml';p.write_text(p.read_text().replace('theseus-comparison:marker',tag));manifest='images'
        elif case=='difference':
            p=root/'archive.toml';p.write_text(p.read_text().replace('chmod 755','printf \'# scoped difference\\n\' >> /home/theseus/.local/bin/comparison-cli; chmod 755',1))
            # Append to each profile's installed binary, keeping the public version/runtime healthy.
            p.write_text(p.read_text().replace('chmod 755 /usr/local/bin','printf \'# scoped difference\\n\' >> /usr/local/bin/comparison-cli; chmod 755 /usr/local/bin'))
            expected=1
        elif case=='failed':
            p=root/'copy.toml';p.write_text(p.read_text().replace('script = "comparison-cli smoke"','script = "comparison-cli smoke; exit 7"'));expected=1
        result=run(root,manifest,expected)
        if case in ['methods','images']:
            assert result['outcome']=='passed';assert len(result['cells'])==12
            assert all(c['comparison']['status']=='equivalent' for c in result['cells'])
            assert all(s['outcome']=='passed' for s in result['sources'])
            assert result['sources'][0]['artifacts']==result['sources'][1]['artifacts'],'input identity differs'
            if case=='images':assert {s['image_id'] for s in result['sources']}=={base,derived}
        elif case=='difference':
            assert result['outcome']=='failed';assert all(s['outcome']=='passed' for s in result['sources'])
            assert {c['profile'] for c in result['cells'] if c['comparison']['status']=='different'}=={'user','system'}
        elif case=='failed':
            assert result['outcome']=='failed';assert result['sources'][0]['outcome']=='failed';assert result['sources'][1]['outcome']=='passed'
            assert all(c['comparison']['status']=='equivalent' for c in result['cells'])
    print('PASS',case,flush=True)
finally:
    try:
        fallback_cleanup(binary, roots, evidence, sys.exc_info()[1])
    finally:
        for tag in owned_tags:command(['docker','image','rm',tag],'remove-owned-marker')
