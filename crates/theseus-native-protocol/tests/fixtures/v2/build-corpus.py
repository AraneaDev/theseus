"""One-off independent Python fixture authoring; never run by Rust tests.

Uses frozen field tables and independent reference arithmetic, not Rust codecs.
Bodies and complete frames are materialized as static files with SHA-256 manifest.
"""
import runpy
import hashlib
import json
from pathlib import Path
ROOT = Path(__file__).parent
r = runpy.run_path(str(ROOT / 'reference-calculation.py'))
compact = r['compact']; ID = r['ID']; DIGEST = r['DIGEST']; U64 = r['U64']
messages = [m for variants in r['messages'].values() for m in variants]
def response(value):
    messages.append(dict(kind='response', reply_to=U64, result=value))
def state(value):
    response(dict(kind='operation', lease=ID, operation=ID, state=value))
messages.append(dict(kind='hello',role='helper',protocol_min=2,protocol_max=2))
for os in ['linux', 'macos']:
    value = dict(r['large_results'][0]); value['os'] = os
    value['helper_principal'] = dict(kind='posix',effective_uid=4294967295,effective_gid=4294967295)
    response(value)
response(dict(kind='acquired', intent=ID,binding=ID,manifest_digest=DIGEST,state=dict(kind='not_found')))
for kind in ['create_file','run']:
    state(dict(kind='accepted',operation_kind=kind))
state(dict(kind='missing')); state(dict(kind='running'))
unknowns = ['helper_restarted_with_unfinished_operation','ledger_write_uncertain','process_state_unproved','file_effect_uncertain']
for reason in unknowns:
    state(dict(kind='unknown',reason=reason))
    response(dict(kind='lease',lease=ID,state=dict(kind='quarantined',intent=ID,reason=reason)))
for code in ['wrong_binding','wrong_manifest','wrong_lease','unsupported','busy','replay_conflict','capacity','ledger_unavailable','ledger_unknown','lease_quarantined','awaiting_external_reset','ownership_pending','invalid_request','not_found']:
    response(dict(kind='refused',code=code))
for code in ['normal','protocol_error','sequence_exhausted','peer_lost','shutdown_deadline','ledger_unavailable']:
    messages.append(dict(kind='closed',code=code))
for code in ['output_truncated','cancel_requested','deadline_expired','cleanup_pending']:
    messages.append(dict(kind='log',lease=ID,operation=ID,code=code))
messages.append(dict(kind='output',lease=ID,operation=ID,channel='stdout',offset=0,bytes='00ff'))
for code in ['io','permission','unsupported','invalid_catalog','native_failure']:
    state(dict(kind='finished',outcome=dict(kind='launch_failed',phase='never_created',code=code,cleanup='no_child_created')))
messages.append(dict(kind='request',request=dict(kind='create_file',lease=ID,operation=ID,path='empty.bin',bytes='')))
response(dict(kind='file_bytes',lease=ID,path='empty.bin',offset=U64,bytes='',eof=True))
response(dict(kind='file_info',lease=ID,path='empty.bin',length=0,digest=DIGEST))
state(dict(kind='finished',outcome=dict(kind='file_created',length=0,digest=DIGEST)))
# Context traces explicitly distinguish each mutation for both counterless reasons.
pairs=[]
for kind in ['create_file','run']:
    request=dict(kind=kind,lease=ID,operation=ID)
    request.update(dict(path='probe.bin',bytes='00ff') if kind=='create_file' else dict(program='probe_success'))
    for reason in ['cancel_requested','deadline']:
        result=dict(kind='operation',lease=ID,operation=ID,state=dict(kind='finished',outcome=dict(kind='not_started',reason=reason)))
        pairs.append(dict(request=request,result=result))
        messages += [dict(kind='request',request=request),dict(kind='response',reply_to=1,result=result)]
manifest=[]
def save(name,body,ledger=False):
    frame=len(body).to_bytes(4,'big')+body+(hashlib.sha256(body).digest() if ledger else b'')
    ROOT.joinpath(name+'.json').write_bytes(body)
    ROOT.joinpath(name+'.frame').write_bytes(frame)
    manifest.append(dict(name=name,body_bytes=len(body),frame_bytes=len(frame),body_sha256=hashlib.sha256(body).hexdigest(),frame_sha256=hashlib.sha256(frame).hexdigest()))
for i,m in enumerate(messages):
    body=compact(dict(version=2,session=ID,sequence=0 if m['kind']=='hello' else U64,message=m))
    save(f'wire-{i:03d}-{m["kind"]}',body)
for kind,variants in r['entries'].items():
    for i,(index,entry) in enumerate(variants):
        save(f'ledger-{kind}-{i:02d}',r['record'](index,entry),True)
ROOT.joinpath('not-started-correlations.json').write_text(json.dumps(pairs,indent=2)+'\n')
ROOT.joinpath('manifest.json').write_text(json.dumps(dict(provenance='Independent Python table authoring; static test inputs, no Rust encode execution.',fixtures=manifest),indent=2)+'\n')
