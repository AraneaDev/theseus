"""Supplemental repository-owned Linux guest-script oracles only.

Never use this helper for the Theseus executable or native host acceptance.
Required real Ubuntu Docker jobs remain the guest-kernel execution evidence.
"""
import hashlib
import json
import os
import pathlib
import subprocess
import sys


def guest_path(path):
    return pathlib.Path(path).resolve().as_posix()


def run_guest_shell(arguments, **kwargs):
    environment = dict(os.environ)
    environment.pop('BASH_ENV', None)
    environment.pop('ENV', None)
    if os.name == 'nt':
        git = pathlib.Path(os.environ['ProgramFiles']) / 'Git'
        shell = git / 'usr/bin/bash.exe'
        if not shell.is_file():
            raise RuntimeError('Required Git for Windows guest-oracle interpreter is missing: ' + str(shell))
        environment['PATH'] = str(git / 'usr/bin') + os.pathsep + environment.get('PATH', '')
        environment['MSYS2_ARG_CONV_EXCL'] = '*'
        command = [str(shell), '--noprofile', '--norc', '--posix']
        git_binary = git / 'cmd/git.exe'
        git_version = subprocess.run([str(git_binary), '--version'], env=environment, check=True, capture_output=True, text=True).stdout
        assert '.windows.' in git_version, 'Required Git for Windows installation identity'
        version = subprocess.run([*command, '--version'], env=environment, check=True, capture_output=True, text=True).stdout
        evidence = dict(scope='MSYS supplemental guest-script contract; not Linux-kernel/native-host proof',
                        executable=str(shell), sha256=hashlib.sha256(shell.read_bytes()).hexdigest(), version=version,
                        git_executable=str(git_binary), git_sha256=hashlib.sha256(git_binary.read_bytes()).hexdigest(), git_version=git_version)
        print(json.dumps(evidence), file=sys.stderr)
        if os.environ.get('THESEUS_HOST_EVIDENCE'):
            (pathlib.Path(os.environ['THESEUS_HOST_EVIDENCE']) / 'guest-oracle-shell.json').write_text(json.dumps(evidence), encoding='utf-8')
    else:
        command = ['/bin/sh']
    return subprocess.run([*command, *arguments], env=environment, **kwargs)
