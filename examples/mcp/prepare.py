#!/usr/bin/env python3
"""Copy an explicitly supplied ELF into private inputs; never execute it on host."""
import argparse
import hashlib
import json
import os
import stat
import sys
from pathlib import Path
import shutil
import tomllib

HERE = Path(__file__).resolve().parent


def scalar(pointer, value):
    return {'kind': 'equals', 'pointer': pointer, 'equals_json': json.dumps(value)}


def absent(pointer):
    return {'kind': 'absent', 'pointer': pointer}


def exact_argv(root, args):
    return [scalar(root + '/command', args[0])] + [scalar(root + '/args/' + str(i), v) for i, v in enumerate(args[1:])] + [absent(root + '/args/' + str(len(args) - 1))]


def match(pointer, key, identity, count, values):
    return {'kind': 'array_match', 'pointer': pointer, 'identity': [{'pointer': key, 'equals_json': json.dumps(identity)}], 'count': count, 'values': [{'pointer': p, 'equals_json': json.dumps(v)} for p, v in values]}


def toml_value(v):
    if isinstance(v, dict):
        return '{ ' + ', '.join(json.dumps(k) + ' = ' + toml_value(x) for k, x in v.items() if x is not None) + ' }'
    if isinstance(v, list):
        return '[' + ', '.join(toml_value(x) for x in v) + ']'
    return json.dumps(v, ensure_ascii=False)


def recipe(args, plugin_recipe=None):
    plugin = args.delivery == 'plugin'
    codex = args.adapter.startswith('codex')
    scopes = ['/client/mcpServers', '/project/mcpServers', '/client/projects/~1workspace/mcpServers']
    slot = {'user': 0, 'project': 1, 'local': 2}.get(args.scope, 0)
    cps = [('configured', 'active', 'target')]
    if args.scenario == 'upgrade':
        cps = [('configured', 'active', 'base'), ('target_installed', 'active', 'target')]
    if plugin:
        cps = [('base_installed', 'active', 'base'), ('target_installed', 'active', 'target'), ('plugin_disabled', 'disabled', 'target'), ('plugin_enabled', 'active', 'target')]
    cps.append(('uninstalled', 'removed', 'target'))
    def script(action, generation):
        return 'sh /inputs/target/action.sh ' + ' '.join([action, args.adapter, args.delivery, args.scope, generation])
    def check(id, cp, action, generation, assertions):
        return {'id': id, 'checkpoint': cp, 'kind': 'json_command', 'script': script(action, generation), 'timeout_seconds': 20, 'expected_exit': 0, 'max_stdout_bytes': 262144, 'assertions': assertions}
    first = cps[0][0]
    checks = [check('mcp-client-version', first, 'version', 'target', [scalar('/version', args.client_version)])]
    contract = {'adapter': args.adapter, 'delivery': args.delivery, 'client_claim': 'configuration_only' if codex else 'native_health', 'server_id': 'probe', 'client_alias': 'plugin:theseus-probe:probe' if plugin else 'probe', 'scope': args.scope, 'client_version': args.client_version, 'client_check': 'mcp-client-version', 'client_artifact': 'client/claude', 'checkpoints': {}}
    for cp, state, generation in cps:
        active, removed = state == 'active', state == 'removed'
        launch = ['/bin/sh', '/workspace/local-market/plugins/theseus-probe/server.sh' if plugin else '/inputs/' + generation + '/server.sh']
        registration, preservation, activation, probe = [cp + '-' + role for role in ['registration', 'preservation', 'activation', 'probe']]
        if codex:
            assertions = [match('/list', '/name', 'probe', 0 if removed else 1, [] if removed else [('/enabled', True)])]
            if not removed:
                assertions += [scalar('/get/name', 'probe'), scalar('/get/enabled', True), scalar('/get/transport/type', 'stdio')] + exact_argv('/get/transport', launch)
            preserved = [scalar('/companion/name', 'companion'), scalar('/companion/enabled', True), scalar('/companion/transport/type', 'stdio')] + exact_argv('/companion/transport', ['/bin/true']) + [scalar('/settings_preserved', True)]
        else:
            assertions = [absent(root + '/probe') for i, root in enumerate(scopes) if plugin or removed or i != slot]
            if plugin:
                identity = 'theseus-probe@theseus-probe-market'
                assertions += [match('/plugins', '/id', identity, 0 if removed else 1, [] if removed else [('/scope', 'user'), ('/enabled', state != 'disabled')])]
                if not removed:
                    # Companion is installed first in the native inventory.
                    root = '/plugins/1'
                    assertions += [scalar(root + '/id', identity), scalar(root + '/readFromFolder', '/workspace/local-market/plugins/theseus-probe')] + exact_argv(root + '/mcpServers/probe', ['/bin/sh', '${CLAUDE_PLUGIN_ROOT}/server.sh'])
            elif not removed:
                assertions += exact_argv(scopes[slot] + '/probe', launch)
            preserved = exact_argv(scopes[0] + '/companion', ['/bin/true']) + [absent(root + '/companion') for root in scopes[1:]] + [scalar('/client/hasCompletedOnboarding', True)]
        checks += [check(registration, cp, 'removed-registration' if removed else 'registration', generation, assertions), check(preservation, cp, 'preservation', generation, preserved)]
        entry = {'state': state, 'registration': registration, 'preservation': [preservation]}
        if not codex:
            checks += [check(activation, cp, 'health' if active else 'inactive-health', generation, [scalar('/status', 0), scalar('/alias', contract['client_alias']), scalar('/source', ' '.join(launch) if active else ''), scalar('/connected', active)])]
            entry['activation'] = activation
        if active:
            version = '1.0.0' if generation == 'base' else '2.0.0'
            checks += [{'id': probe, 'checkpoint': cp, 'kind': 'mcp_stdio', 'probe': {'argv': launch, 'protocol': args.protocol, 'timeout_seconds': 15, 'server_name': 'theseus-fixture', 'server_version': version, 'tools': ['hello_v1' if generation == 'base' else 'hello_v2']}}]
            entry['probe'] = probe
        contract['checkpoints'][cp] = entry
    if plugin:
        result = plugin_recipe
        profile = result['profiles']['user']
        profile['checks'] += checks
        for step in profile['steps']:
            profile['steps'][step]['script'] = script(step, 'base' if step == 'install_base' else 'target')
    else:
        steps = ['install_target', 'configure', 'exercise', 'uninstall'] if args.scenario == 'clean' else ['install_base', 'configure', 'upgrade', 'exercise', 'uninstall']
        profile = {'steps': {s: {'script': script(s, 'base' if args.scenario == 'upgrade' and s in ['install_base', 'configure'] else 'target')} for s in steps}, 'checks': checks}
        result = {'name': 'Supplied native MCP fixture: ' + args.adapter, 'image': args.image, 'network': 'none', 'artifacts': {'target': 'target'}, 'profiles': {'user': profile}, 'observe': ['/workspace', '/home/theseus']}
        if args.scenario == 'upgrade':
            result['artifacts']['base'] = 'base'
    result['schema_version'] = 8
    result['scenarios'] = [args.scenario]
    profile['mcp'] = contract
    return result


def open_client(source):
    # Unix retains atomic leaf no-follow/nonblocking acquisition. Windows uses
    # checked lstat/open/lstat identities; this is not atomic no-follow or a
    # sandbox against same-user ancestor replacement.
    if os.name == 'nt' and sys.version_info < (3, 12):
        raise ValueError('Windows preparation requires Python 3.12+ native file identity')
    before = source.lstat()
    flags = os.O_RDONLY
    if os.name == 'nt':
        flags |= os.O_BINARY | os.O_NOINHERIT
    else:
        flags |= os.O_NOFOLLOW | os.O_NONBLOCK
    fd = os.open(source, flags)
    try:
        opened, after = os.fstat(fd), source.lstat()
        for info in [before, opened, after]:
            if not stat.S_ISREG(info.st_mode):
                raise ValueError('client must be a regular nonsymlink file')
            if os.name == 'nt':
                if not hasattr(info, 'st_file_attributes') or info.st_file_attributes & stat.FILE_ATTRIBUTE_REPARSE_POINT:
                    raise ValueError('client reparse attributes are missing or unsafe')
                if not info.st_ino or not info.st_dev:
                    raise ValueError('client native identity is unavailable')
        if not ((before.st_dev, before.st_ino) == (opened.st_dev, opened.st_ino) == (after.st_dev, after.st_ino)):
            raise ValueError('client identity changed during acquisition')
        stream = os.fdopen(fd, 'rb')
    except BaseException:
        os.close(fd)
        raise
    return stream


def prepare(args):
    expected = {'claude-code-2.1.312': '2.1.312 (Claude Code)', 'codex-cli-0.166.0': 'codex-cli 0.166.0'}
    if args.client_version != expected[args.adapter]:
        raise ValueError('adapter requires exact expected native client version')
    plugin, codex = args.delivery == 'plugin', args.adapter.startswith('codex')
    if (plugin and (codex or args.scope != 'user' or args.scenario != 'plugin_lifecycle')) or (not plugin and args.scenario not in ['clean', 'upgrade']) or (codex and args.scope != 'global') or (not codex and args.scope not in ['user', 'project', 'local']):
        raise ValueError('unsupported adapter/delivery/scope/scenario combination')
    output = Path(args.output).absolute()
    checkout = HERE.parent.parent
    for tree in [checkout / 'examples', checkout / 'tests']:
        if output.resolve() == tree or tree in output.resolve().parents:
            raise ValueError('output must be outside checkout examples/tests')
    if output.exists() or output.is_symlink():
        raise ValueError('output must be a new directory')
    source = Path(args.client).absolute()
    if source.is_symlink() or not source.is_file():
        raise ValueError('client must be an explicit regular nonsymlink ELF')
    if not args.image or any(ord(c) < 32 or ord(c) > 126 for c in args.image):
        raise ValueError('image must be printable ASCII')
    with open_client(source) as stream:
        if os.fstat(stream.fileno()).st_mode & 0o170000 != 0o100000:
            raise ValueError('client must be a regular file')
        first = stream.read(4)
        if first != b'\x7fELF':
            raise ValueError('client must have ELF header; synthetic replay is test-only')
        output.mkdir(mode=0o700)
        try:
            target = output / 'target'
            if plugin:
                shutil.copytree(HERE.parent / 'plugins/fixture', target)
            else:
                target.mkdir()
            (target / 'client').mkdir()
            digest, size = hashlib.sha256(), 0
            with (target / 'client/claude').open('xb') as destination:
                chunk = first
                while chunk:
                    destination.write(chunk); digest.update(chunk); size += len(chunk)
                    chunk = stream.read(1024 * 1024)
            (target / 'client/claude').chmod(0o755)
            for asset in ['action.sh', 'codex-settings-prefix.txt']:
                shutil.copyfile(HERE / 'assets' / asset, target / asset)
            (target / 'expected-version.txt').write_text(args.client_version + '\n', encoding='utf-8', newline='\n')
            for generation, version, tool in [('base', '1.0.0', 'hello_v1'), ('target', '2.0.0', 'hello_v2')]:
                server = (HERE / 'assets/server.sh').read_text(encoding='utf-8').replace('@@VERSION@@', version).replace('@@TOOL@@', tool).replace('@@PROTOCOL@@', args.protocol)
                if plugin:
                    folder = target / ('market-v1' if generation == 'base' else 'market-v2') / 'plugins/theseus-probe'
                    (folder / 'server.sh').write_text(server, encoding='utf-8', newline='\n')
                    (folder / '.mcp.json').write_text(json.dumps({'probe': {'command': '/bin/sh', 'args': ['${CLAUDE_PLUGIN_ROOT}/server.sh']}}) + '\n', encoding='utf-8', newline='\n')
                else:
                    folder = output / generation
                    folder.mkdir(exist_ok=True)
                    (folder / 'server.sh').write_text(server, encoding='utf-8', newline='\n')
            plugin_recipe = None
            if plugin:
                text = (HERE.parent / 'plugins/user.toml.in').read_text(encoding='utf-8').replace('@@IMAGE_JSON@@', json.dumps(args.image)).replace('@@CLIENT_VERSION_JSON@@', json.dumps(args.client_version)).replace('@@CLIENT_VERSION_SCALAR_JSON@@', json.dumps(json.dumps(args.client_version)))
                text = text.replace('/workspace/plugin-project', '/workspace')
                plugin_recipe = tomllib.loads(text)
                for name in ['lifecycle.sh', 'client-version.sh', 'components.sh']:
                    path = target / name
                    path.write_text(path.read_text(encoding='utf-8').replace('/workspace/plugin-project', '/workspace'), encoding='utf-8', newline='\n')
            result = recipe(args, plugin_recipe)
            (output / 'theseus.toml').write_text('\n'.join(json.dumps(k) + ' = ' + toml_value(v) for k, v in result.items()) + '\n', encoding='utf-8', newline='\n')
            manifest = {'provenance': 'explicit supplied ELF; never executed on host; expected version until guest proof', 'adapter': args.adapter, 'scope': args.scope, 'delivery': args.delivery, 'scenario': args.scenario, 'protocol': args.protocol, 'image': args.image, 'client': {'path': 'target/client/claude', 'sha256': digest.hexdigest(), 'size': size, 'expected_version': args.client_version}}
            (output / 'input-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8', newline='\n')
        except BaseException:
            shutil.rmtree(output)
            raise


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--client', required=True)
    p.add_argument('--client-version', required=True)
    p.add_argument('--adapter', choices=['claude-code-2.1.312', 'codex-cli-0.166.0'], required=True)
    p.add_argument('--delivery', choices=['standalone', 'plugin'], required=True)
    p.add_argument('--scope', choices=['user', 'project', 'local', 'global'], required=True)
    p.add_argument('--scenario', choices=['clean', 'upgrade', 'plugin_lifecycle'], required=True)
    p.add_argument('--protocol', choices=['2026-07-28', '2025-11-25'], required=True)
    p.add_argument('--image', default='ubuntu:24.04')
    p.add_argument('--output', required=True)
    args = p.parse_args()
    try:
        prepare(args)
    except (ValueError, OSError) as e:
        p.exit(1, 'preparation failed: ' + str(e) + '\n')


if __name__ == '__main__':
    main()
