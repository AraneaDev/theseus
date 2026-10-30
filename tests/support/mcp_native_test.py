"""Host-only harness oracles: literal frames; never Docker or native client execution."""
import copy
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import mcp_native as native

MODERN = [
 {'jsonrpc':'2.0','id':'theseus-discover-1','result':{'resultType':'complete','supportedVersions':['2026-07-28'],'capabilities':{'tools':{}},'ttlMs':0,'cacheScope':'public','_meta':{'io.modelcontextprotocol/serverInfo':{'name':'theseus-fixture','version':'2.0.0'}}}},
 {'jsonrpc':'2.0','id':1,'result':{'resultType':'complete','ttlMs':0,'cacheScope':'public','tools':[{'name':'hello_v2','inputSchema':{'type':'object'}}]}}
]

class WireOracle(unittest.TestCase):
    def test_independent_received_response_inventory_and_identity(self):
        lines = [json.dumps(v) + '\n' for v in MODERN]
        observed = native.wire_facts(lines, '2026-07-28')
        self.assertEqual(observed, {'identity': {'name':'theseus-fixture','version':'2.0.0'}, 'tools':['hello_v2'], 'protocol':'2026-07-28'})
        wrong = copy.deepcopy(MODERN)
        wrong[1]['result']['tools'][0]['name'] = 'actually_wrong'
        self.assertEqual(native.wire_facts([json.dumps(v)+'\n' for v in wrong], '2026-07-28')['tools'], ['actually_wrong'])
        self.assertRaises(AssertionError, native.expect_inventory, observed, '1.0.0', ['hello_v1'])

    def test_health_capture_retains_original_failure_status_and_raw_native_diagnostics(self):
        import shlex
        with tempfile.TemporaryDirectory() as directory:
            capture=Path(directory)/'health.capture'
            capture.write_text('probe: actual failure detail\n')
            # Explicit synthetic shell producer, never a native client/server.
            script=native.health_capture("printf receipt; (exit 17)",str(capture))
            result=native.subprocess.run(['sh','-c',script],capture_output=True,text=True)
            self.assertEqual(result.returncode,17)
            self.assertEqual(result.stdout,'receipt')
            self.assertEqual(result.stderr,'probe: actual failure detail\n')

    def test_native_registration_oracle_rejects_cross_scope_duplicate_and_wrong_plugin_source(self):
        contract={'delivery':'standalone','scope':'user','adapter':'claude-code-2.1.312'}
        doc={'client':{'mcpServers':{'probe':{'command':'/bin/sh','args':['/inputs/target/server.sh']}},'projects':{'/workspace':{'mcpServers':{}}}},'project':{'mcpServers':{}}}
        native.registration_facts(doc,contract,'configured','clean')
        doc['project']['mcpServers']['probe']=doc['client']['mcpServers']['probe']
        self.assertRaises(AssertionError,native.registration_facts,doc,contract,'configured','clean')
        doc['project']['mcpServers'].clear();doc['client']['mcpServers'].clear()
        contract.update(delivery='plugin')
        target={'id':'theseus-probe@theseus-probe-market','scope':'user','enabled':True,'readFromFolder':'/workspace/local-market/plugins/theseus-probe','mcpServers':{'probe':{'command':'/bin/sh','args':['${CLAUDE_PLUGIN_ROOT}/server.sh']}}}
        doc['plugins']=[{'id':'companion@market'},target]
        native.registration_facts(doc,contract,'target_installed','plugin_lifecycle')
        target['readFromFolder']='/cache/unrelated'
        self.assertRaises(AssertionError,native.registration_facts,doc,contract,'target_installed','plugin_lifecycle')

    def test_malformed_duplicate_id_and_unfinished_pages_cannot_satisfy_oracle(self):
        cases = [
            ['{invalid\n'],
            ['{"jsonrpc":"2.0","jsonrpc":"2.0","id":1,"result":{}}\n'],
            [json.dumps(MODERN[0])+'\n', json.dumps(MODERN[0])+'\n'],
        ]
        unfinished = copy.deepcopy(MODERN)
        unfinished[1]['result']['nextCursor'] = 'more'
        cases.append([json.dumps(v)+'\n' for v in unfinished])
        for lines in cases:
            with self.subTest(lines=lines):
                self.assertRaises((AssertionError, ValueError), native.wire_facts, lines, '2026-07-28')

    def test_legacy_is_independent_and_rejects_modern_startup(self):
        lines = [json.dumps({'jsonrpc':'2.0','id':'theseus-initialize-1','result':{'protocolVersion':'2025-11-25','capabilities':{'tools':{}},'serverInfo':{'name':'theseus-fixture','version':'1.0.0'}}})+'\n',
                 '{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"hello_v1","inputSchema":{"type":"object"}}]}}\n']
        native.expect_inventory(native.wire_facts(lines, '2025-11-25'), '1.0.0', ['hello_v1'])
        self.assertRaises(AssertionError, native.wire_facts, lines, '2026-07-28')

    def test_missing_inputs_fail_explicitly_before_docker(self):
        with patch.dict(os.environ, {}, clear=True), patch.object(native.subprocess, 'run') as launch:
            with self.assertRaisesRegex(AssertionError, 'UNRUN.*THESEUS_MCP'):
                native.prerequisites()
            launch.assert_not_called()

    def test_cancel_marker_requires_unique_target_direct_request_uid_and_stream(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / 'c019-stderr.log'
            (root / 'c019-stdout.log').write_bytes(b'')
            marker = b'THESEUS_DIRECT_HOLD token=unique uid=10001 request={"jsonrpc":"2.0","id":"theseus-discover-1","method":"server/discover"}\n'
            for invalid in [marker.replace(b'unique', b'earlier'), marker.replace(b'10001', b'0'), marker.replace(b'theseus-discover-1', b'native-1')]:
                log.write_bytes(invalid)
                self.assertIsNone(native.direct_ready(root, 'unique', '2026-07-28'))
            log.write_bytes(marker)
            ready = native.direct_ready(root, 'unique', '2026-07-28')
            self.assertEqual(ready['stderr_path'], str(log))
            self.assertEqual(ready['request']['id'], 'theseus-discover-1')
            self.assertEqual(ready['stdout_bytes_at_ready'], 0)
            log.rename(root / 'c019-stdout.log')
            self.assertIsNone(native.direct_ready(root, 'unique', '2026-07-28'))

class CurrentDetailOracle(unittest.TestCase):
    def test_resize_updates_actual_terminal_geometry(self):
        import fcntl
        import pty
        import struct
        import termios
        import types
        from mcp_native_pty import McpSession
        master,slave=pty.openpty()
        try:
            session=McpSession.__new__(McpSession)
            session.slave=slave;session.steps=[]
            session.proc=types.SimpleNamespace(pid=os.getpid())
            # This unit verifies the real ioctl/geometry boundary. The separate
            # public PTYs verify redraw/current cells, never this replaced wait.
            session.until=lambda *args:None
            with patch('mcp_native_pty.os.kill'):
                session.resize(36,120)
            rows,columns,_,_=struct.unpack('HHHH',fcntl.ioctl(slave,termios.TIOCGWINSZ,b'\0'*8))
            self.assertEqual((rows,columns),(36,120))
            self.assertEqual((session.screen.rows,session.screen.columns),(36,120))
        finally:
            os.close(master);os.close(slave)

    def test_list_names_and_old_cells_do_not_substitute_for_selected_wrapped_detail(self):
        from mcp_native_pty import McpSession
        from terminal_screen import Screen
        session = McpSession.__new__(McpSession)
        session.screen = Screen(8, 60)
        session.screen.feed(('\x1b[2J\x1b[1;1HRecords: hello_v2\x1b[2;1H┌Detail — record 1 / 1' + '─'*36 + '┐' +
                             '\x1b[3;1H│independent typed stdio' + ' '*34 + '│' +
                             '\x1b[4;1H│discovery hello_v1' + ' '*40 + '│' +
                             '\x1b[5;1H└' + '─'*58 + '┘').encode())
        self.assertTrue(session.detail_contains('independent typed stdio discovery hello_v1'))
        self.assertFalse(session.detail_contains('hello_v2'))
        session.screen.feed(b'\x1b[2J\x1b[1;1HRecords: hello_v1')
        self.assertFalse(session.detail_contains('hello_v1'))

if __name__ == '__main__':
    unittest.main()
