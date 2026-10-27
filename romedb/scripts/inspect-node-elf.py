#!/usr/bin/env python3
"""Record Linux addon ELF requirements without loading the binary (Python 3.11+)."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('addon', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    details = subprocess.check_output(
        ['readelf', '--wide', '--file-header', '--dynamic', '--version-info', str(args.addon)],
        text=True, timeout=30, env={**os.environ, 'LC_ALL': 'C'},
    )
    def header(name):
        match = re.search(r'^\s*' + re.escape(name) + r':\s*(.+)$', details, re.M)
        if not match:
            raise ValueError(f'Missing ELF header field: {name}')
        return match.group(1).strip()
    if not header('Type').startswith('DYN '):
        raise ValueError('Expected a shared-object ELF addon')
    requirements = {}
    library = None
    # Version definitions are exports; only version needs describe dependencies.
    needs = details.partition('Version needs section')[2]
    for line in needs.splitlines():
        match = re.search(r'File:\s*(\S+)', line)
        if match:
            library = match.group(1)
            requirements.setdefault(library, [])
        match = re.search(r'Name:\s*(\S+)', line)
        if match and library:
            requirements[library].append(match.group(1))
    versions = sorted({v for values in requirements.values() for v in values if re.fullmatch(r'GLIBC_[0-9.]+', v)},
                      key=lambda value: tuple(int(part) for part in value[6:].split('.')))
    with args.addon.open('rb') as stream:
        checksum = hashlib.file_digest(stream, 'sha256').hexdigest()
    result = {
        'schemaVersion': 1,
        'scope': 'Observed ELF dependencies; not proof of compatibility with any Linux distribution or Node version',
        'bytes': args.addon.stat().st_size,
        'sha256': checksum,
        'elfClass': header('Class'),
        'machine': header('Machine'),
        'neededLibraries': sorted(set(re.findall(r'\(NEEDED\).*?\[([^]]+)\]', details))),
        'searchPaths': re.findall(r'\((?:RPATH|RUNPATH)\).*?\[([^]]*)\]', details),
        'requiredSymbolVersions': {name: sorted(set(values)) for name, values in sorted(requirements.items())},
        'maximumReferencedGlibcVersion': versions[-1][6:] if versions else None,
    }
    serialized = json.dumps(result, indent=2) + '\n'
    if args.check:
        if args.output.read_text() != serialized:
            raise ValueError('ELF report is stale for this addon')
    else:
        args.output.write_text(serialized)
    print(f'Verified {result["elfClass"]} addon; maximum referenced GLIBC symbol version: {result["maximumReferencedGlibcVersion"]}')


if __name__ == '__main__':
    main()
