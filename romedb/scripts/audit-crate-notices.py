#!/usr/bin/env python3
"""Inventory notice candidates in locked cached crates without extracting them.

Requires Python 3.11+. This audits source evidence, not license compliance or
which components are present in a compiled binary.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
NOTICE = re.compile(r"^(LICENSE|LICENCE|COPYING|NOTICE|COPYRIGHT)(?:[._-].*)?$", re.I)
MAX_NOTICE_BYTES = 4 * 1024 * 1024
MAX_INLINE_SOURCE_BYTES = 200_000
INLINE_MARKER = re.compile(r"copyright|SPDX-License|licensed under|is licensed under", re.I)
INLINE_SUFFIXES = {'.md', '.rst', '.txt', '.rs', '.c', '.cc', '.cpp', '.cxx', '.h', '.hpp', '.hxx'}


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def inspect_archive(archive, checksum, name, version):
    with archive.open('rb') as stream:
        actual = hashlib.file_digest(stream, 'sha256').hexdigest()
    if actual != checksum:
        raise ValueError(f'Archive checksum mismatch: {name}@{version}')
    prefix = f'{name}-{version}/'
    with tarfile.open(archive, 'r:gz') as crate:
        manifest = crate.extractfile(prefix + 'Cargo.toml')
        if manifest is None:
            raise ValueError(f'Missing crate manifest: {name}@{version}')
        package = tomllib.loads(manifest.read().decode())['package']
        if package['name'] != name or package['version'] != version:
            raise ValueError(f'Archive identity mismatch: {name}@{version}')
        declared = package.get('license-file')
        notices = []
        for member in crate.getmembers():
            if not member.isfile() or not member.name.startswith(prefix):
                continue
            relative = member.name[len(prefix):]
            if relative != declared and not NOTICE.match(PurePosixPath(relative).name):
                continue
            if member.size > MAX_NOTICE_BYTES:
                raise ValueError(f'Notice candidate exceeds audit bound: {name}/{relative}')
            with crate.extractfile(member) as stream:
                contents = stream.read(MAX_NOTICE_BYTES + 1)
            if len(contents) > MAX_NOTICE_BYTES:
                raise ValueError(f'Notice candidate exceeds audit bound: {name}/{relative}')
            notices.append({'path': relative, 'bytes': len(contents), 'sha256': sha256(contents)})
        inline = []
        if not notices:
            for member in crate.getmembers():
                if not member.isfile() or not member.name.startswith(prefix):
                    continue
                relative = member.name[len(prefix):]
                if member.size > MAX_INLINE_SOURCE_BYTES or PurePosixPath(relative).suffix.lower() not in INLINE_SUFFIXES:
                    continue
                with crate.extractfile(member) as stream:
                    contents = stream.read(MAX_INLINE_SOURCE_BYTES + 1)
                if len(contents) > MAX_INLINE_SOURCE_BYTES:
                    continue
                markers = [
                    {'line': number, 'text': line[:240]}
                    for number, line in enumerate(contents.decode('utf8', errors='replace').splitlines(), 1)
                    if INLINE_MARKER.search(line)
                ]
                if markers:
                    inline.append({'path': relative, 'sha256': sha256(contents), 'markers': markers})
    result = {
        'status': 'archive_verified',
        'archiveSha256': actual,
        'declaredLicenseFile': declared,
        'noticeCandidates': sorted(notices, key=lambda item: item['path']),
    }
    if not notices:
        result['inlineSearchScope'] = 'Code/text files at most 200000 bytes; selected suffixes and marker phrases; not exhaustive'
        result['inlineNoticeCandidates'] = sorted(inline, key=lambda item: item['path'])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('inventory', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    inventory_bytes = args.inventory.read_bytes()
    inventory = json.loads(inventory_bytes)
    lock_bytes = (ROOT / 'Cargo.lock').read_bytes()
    if inventory['lockfileSha256'] != sha256(lock_bytes):
        raise ValueError('Dependency inventory has a stale lockfile hash')
    locked = tomllib.loads(lock_bytes.decode())['package']
    cache = Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo')) / 'registry/cache'
    results = []
    for item in inventory['packages']:
        name, version = item['name'], item['version']
        matches = [p for p in locked if p['name'] == name and p['version'] == version]
        if len(matches) != 1:
            raise ValueError(f'Ambiguous or missing lockfile identity: {name}@{version}')
        package = matches[0]
        result = {'name': name, 'version': version}
        if 'source' not in package:
            result['status'] = 'workspace_source_requires_separate_review'
        elif not package['source'].startswith('registry+'):
            raise ValueError(f'Unsupported source requires separate review: {name}@{version}')
        else:
            archives = sorted(cache.glob(f'*/{name}-{version}.crate'))
            if not archives:
                raise ValueError(f'Missing cached archive: {name}@{version}')
            # Identical identities in multiple registries need explicit resolution.
            if len(archives) != 1:
                raise ValueError(f'Ambiguous cached archive: {name}@{version}')
            result.update(inspect_archive(archives[0], package['checksum'], name, version))
        results.append(result)
    report = {
        'schemaVersion': 1,
        'target': inventory['target'],
        'inventorySha256': sha256(inventory_bytes),
        'scope': 'Checksummed crate archive notice candidates; not complete notices or linked-binary evidence',
        'packages': results,
    }
    serialized = json.dumps(report, indent=2) + '\n'
    if args.check:
        if args.output.read_text() != serialized:
            raise ValueError('Crate notice audit is stale; regenerate it')
    else:
        args.output.write_text(serialized)
    count = sum(p['status'] == 'archive_verified' for p in results)
    print(f'Verified {count} cached crate archives; {len(results) - count} workspace packages require separate review')


if __name__ == '__main__':
    main()
