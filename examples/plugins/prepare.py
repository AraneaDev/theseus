#!/usr/bin/env python3
"""Stage only an explicitly supplied native ELF file; never launch it on the host."""
import argparse
import hashlib
import json
import os
import sys
from pathlib import Path
import shutil

# This private support module belongs to this checkout, never the supplied input
# or caller's current directory. Keep the existing public CLI entry points.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
try:
    from _client_input import open_client
finally:
    sys.path.pop(0)


def printable_version(value):
    alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789. ()_-'
    return 1 <= len(value) <= 128 and all(c in alphabet for c in value)


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
