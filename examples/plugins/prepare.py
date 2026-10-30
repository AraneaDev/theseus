#!/usr/bin/env python3
"""Stage only an explicitly supplied native ELF file; never launch it on the host."""
import argparse
import hashlib
import json
import os
import stat
import sys
from pathlib import Path
import shutil


def printable_version(value):
    alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789. ()_-'
    return 1 <= len(value) <= 128 and all(c in alphabet for c in value)


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
    source = Path(args.client).absolute()
    output = Path(args.output).absolute()
    templates = Path(__file__).resolve().parent
    destination = output.resolve()
    if output.exists() or output.is_symlink():
        raise ValueError('output must be a new directory')
    checkout = templates.parent.parent
    for tree in (checkout / 'examples', checkout / 'tests'):
        if destination == tree or tree in destination.parents:
            raise ValueError('output must be outside the checkout examples/tests source trees')
    if source.resolve() in destination.parents:
        raise ValueError('output must not be nested inside the supplied client')
    if source.is_symlink() or not source.is_file():
        raise ValueError('client must be an explicit regular nonsymlink file')
    if not printable_version(args.client_version):
        raise ValueError('expected version must be 1..128 characters from A-Za-z0-9. ()_-')
    if not args.image or any(ord(c) < 32 or ord(c) > 126 for c in args.image):
        raise ValueError('image must be nonempty printable ASCII')
    with open_client(source) as client:
        if os.fstat(client.fileno()).st_mode & 0o170000 != 0o100000:
            raise ValueError('client must be a regular file')
        first = client.read(4)
        if first != b'\x7fELF':
            raise ValueError('client must have an ELF header; shell replay is test-only')
        # The exclusive mkdir establishes ownership for failure cleanup.
        output.mkdir(mode=0o700)
        try:
            target = output / 'target'
            shutil.copytree(templates / 'fixture', target)
            (target / 'client').mkdir()
            digest = hashlib.sha256()
            size = 0
            with (target / 'client/claude').open('xb') as staged:
                chunk = first
                while chunk:
                    staged.write(chunk)
                    digest.update(chunk)
                    size += len(chunk)
                    chunk = client.read(1024 * 1024)
            (target / 'client/claude').chmod(0o755)
            template = (templates / (args.scope + '.toml.in')).read_text(encoding='utf-8')
            rendered = template.replace('@@IMAGE_JSON@@', json.dumps(args.image))
            rendered = rendered.replace('@@CLIENT_VERSION_JSON@@', json.dumps(args.client_version))
            rendered = rendered.replace('@@CLIENT_VERSION_SCALAR_JSON@@', json.dumps(json.dumps(args.client_version)))
            (output / 'theseus.toml').write_text(rendered, encoding='utf-8', newline='\n')
            manifest = {'provenance': 'explicit supplied ELF; not executed on host',
                        'scope': args.scope, 'image': args.image,
                        'client': {'path': 'target/client/claude', 'sha256': digest.hexdigest(),
                                   'size': size, 'expected_version': args.client_version,
                                   'version_status': 'expected until guest verification'}}
            (output / 'input-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8', newline='\n')
        except BaseException:
            shutil.rmtree(output)
            raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--client', required=True)
    parser.add_argument('--client-version', required=True)
    parser.add_argument('--scope', choices=['user', 'project', 'local'], default='user')
    parser.add_argument('--output', required=True)
    parser.add_argument('--image', default='ubuntu:24.04')
    args = parser.parse_args()
    try:
        prepare(args)
    except (OSError, ValueError) as error:
        parser.exit(1, 'preparation failed: ' + str(error) + '\n')


if __name__ == '__main__':
    main()
