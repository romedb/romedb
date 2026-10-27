#!/usr/bin/env python3
"""Bundle verified crate notice candidates; does not claim a complete audit."""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]


def digest(data):
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('inventory', type=Path)
    parser.add_argument('audit', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--require-complete', action='store_true', help='Reject packages without archive, supplemental or declared-license texts')
    parser.add_argument('--supplements', type=Path, help='Pinned repository notice sources with local text files')
    args = parser.parse_args()
    inventory_bytes = args.inventory.read_bytes()
    inventory = json.loads(inventory_bytes)
    audit = json.loads(args.audit.read_bytes())
    lock = (ROOT / 'Cargo.lock').read_bytes()
    if inventory['lockfileSha256'] != digest(lock) or audit['inventorySha256'] != digest(inventory_bytes):
        raise ValueError('Stale inventory or notice audit')
    identities = lambda packages: [(p['name'], p['version']) for p in packages]
    if identities(inventory['packages']) != identities(audit['packages']):
        raise ValueError('Audit package identities differ from inventory')
    locked = tomllib.loads(lock.decode())['package']
    cache = Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo')) / 'registry/cache'
    supplements = {}
    workspace_entries = {}
    declarations = {}
    if args.supplements:
        manifest = json.loads(args.supplements.read_bytes())
        if manifest['schemaVersion'] != 1:
            raise ValueError('Unsupported supplement schema')
        for entry in manifest.get('workspaceEntries', []):
            key = (entry['package'], entry['version'])
            if key in workspace_entries:
                raise ValueError(f'Duplicate workspace notice entry: {key}')
            workspace_entries[key] = entry
        for entry in manifest['entries']:
            supplements.setdefault((entry['package'], entry['version']), []).append(entry)
        for entry in manifest.get('declarationEntries', []):
            key = (entry['package'], entry['version'])
            if key in declarations:
                raise ValueError(f'Duplicate declaration entry: {key}')
            declarations[key] = entry
    texts = {}
    entries = []
    unresolved = []
    for package in audit['packages']:
        name, version = package['name'], package['version']
        label = f'{name} {version}'
        workspace = workspace_entries.get((name, version))
        if workspace:
            matches = [p for p in locked if p['name'] == name and p['version'] == version]
            if len(matches) != 1 or 'source' in matches[0]:
                raise ValueError(f'Workspace notice entry is not a local locked package: {label}')
            def local_path(relative):
                path = (ROOT / relative).resolve()
                if not path.is_relative_to(ROOT.resolve()):
                    raise ValueError(f'Workspace notice path escapes repository: {label}')
                return path
            source = tomllib.loads(local_path(workspace['manifestPath']).read_text())['package']
            inherited = tomllib.loads((ROOT / 'Cargo.toml').read_text()).get('workspace', {}).get('package', {})
            def package_field(field):
                value = source.get(field)
                return inherited.get(field) if isinstance(value, dict) and value.get('workspace') else value
            if (source['name'] != name or package_field('version') != version
                    or package_field('license') != workspace['licenseExpression']):
                raise ValueError(f'Workspace package identity or license changed: {label}')
            if not workspace['noticeFiles']:
                raise ValueError(f'Workspace entry has no notice texts: {label}')
            entries.append(f'\n### {label}\n')
            for item in workspace['noticeFiles']:
                path = local_path(item['path'])
                if path.stat().st_size > 4 * 1024 * 1024:
                    raise ValueError(f'Workspace notice exceeds size bound: {label}')
                data = path.read_bytes()
                key = item['sha256']
                if digest(data) != key:
                    raise ValueError(f'Workspace notice checksum mismatch: {label}')
                texts[key] = data.decode('utf8')
                entries.append(f'- Workspace `{item["path"]}`: [source text {key}](#text-{key})\n')
            continue
        files = package.get('noticeCandidates', [])
        extra = supplements.get((name, version), [])
        declaration = declarations.get((name, version))
        if not files and not extra and not declaration:
            unresolved.append(label)
            continue
        matches = [p for p in locked if p['name'] == name and p['version'] == version]
        if len(matches) != 1 or matches[0].get('checksum') != package.get('archiveSha256'):
            raise ValueError(f'Archive is not pinned by lockfile: {label}')
        archives = sorted(cache.glob(f'*/{name}-{version}.crate'))
        if len(archives) != 1:
            raise ValueError(f'Missing or ambiguous cached archive: {label}')
        with archives[0].open('rb') as stream:
            if hashlib.file_digest(stream, 'sha256').hexdigest() != package['archiveSha256']:
                raise ValueError(f'Archive checksum mismatch: {label}')
        entries.append(f'\n### {label}\n')
        with tarfile.open(archives[0], 'r:gz') as crate:
            if declaration:
                if declaration['archiveSha256'] != package['archiveSha256']:
                    raise ValueError(f'Declaration archive mismatch: {label}')
                data = crate.extractfile(f'{name}-{version}/Cargo.toml').read()
                source = tomllib.loads(data.decode())['package']
                expression = declaration['licenseExpression']
                choices = re.split(r'\s+OR\s+|\s*/\s*', expression)
                selected = declaration['selectedLicense']
                if (source['name'] != name or source['version'] != version
                        or source.get('license') != expression
                        or selected not in choices
                        or not all(re.fullmatch(r'[A-Za-z0-9.+-]+', c) for c in choices)):
                    raise ValueError(f'Declaration license selection mismatch: {label}')
                sources = declaration['textSources']
                if not any(s.get('licenseId') == selected for s in sources):
                    raise ValueError(f'Declaration lacks selected license text: {label}')
                key = digest(data)
                texts[key] = data.decode()
                entries.append(f'- Archived Cargo manifest declares `{expression}`; distribution selects `{selected}`. '
                               f'[Original declaration and author metadata](#text-{key}).\n')
                entries.append('- Standard license text below is a reference for this declaration, '
                               'not a fabricated upstream LICENSE file or copyright attribution.\n')
                for item in sources:
                    path = (args.supplements.parent / item['sourceFile']).resolve()
                    if not path.is_relative_to(args.supplements.parent.resolve()):
                        raise ValueError(f'Declaration text path escapes manifest: {label}')
                    data = path.read_bytes()
                    key = item['sourceSha256']
                    if len(data) > 4 * 1024 * 1024 or digest(data) != key:
                        raise ValueError(f'Declaration text checksum mismatch: {label}')
                    texts[key] = data.decode()
                    entries.append(f'- Reference [{item["sourceUrl"]}]({item["sourceUrl"]}): [text {key}](#text-{key})\n')
                for item in declaration.get('archiveTexts', []):
                    member = crate.getmember(f'{name}-{version}/' + item['path'])
                    if not member.isfile() or member.size > 4 * 1024 * 1024:
                        raise ValueError(f'Invalid declaration archive text: {label}')
                    data = crate.extractfile(member).read()
                    key = item['sha256']
                    if digest(data) != key:
                        raise ValueError(f'Declaration archive text checksum mismatch: {label}')
                    texts[key] = data.decode()
                    entries.append(f'- Original `{item["path"]}`: [text {key}](#text-{key})\n')
            if extra:
                with crate.extractfile(f'{name}-{version}/.cargo_vcs_info.json') as stream:
                    vcs = json.load(stream)
                for item in extra:
                    if (vcs['git']['sha1'] != item['sourceRevision']
                            or vcs.get('path_in_vcs', '') != item['cratePathInSource']
                            or item['sourceRevision'] not in item['sourceUrl'].split('/')):
                        raise ValueError(f'Supplement revision differs from crate: {label}')
                    path = (args.supplements.parent / item['sourceFile']).resolve()
                    if not path.is_relative_to(args.supplements.parent.resolve()):
                        raise ValueError(f'Supplement path escapes manifest directory: {label}')
                    data = path.read_bytes()
                    key = item['sourceSha256']
                    if len(data) > 4 * 1024 * 1024 or digest(data) != key:
                        raise ValueError(f'Supplement checksum mismatch: {label}')
                    texts[key] = data.decode('utf8')
                    entries.append(f'- Repository source [{item["sourceUrl"]}]({item["sourceUrl"]}): '
                                   f'[source text {key}](#text-{key})\n')
            for item in files:
                member = crate.getmember(f'{name}-{version}/' + item['path'])
                if not member.isfile() or member.size != item['bytes'] or member.size > 4 * 1024 * 1024:
                    raise ValueError(f'Invalid notice candidate: {label}/{item["path"]}')
                with crate.extractfile(member) as stream:
                    data = stream.read(4 * 1024 * 1024 + 1)
                if digest(data) != item['sha256']:
                    raise ValueError(f'Notice checksum mismatch: {label}/{item["path"]}')
                key = item['sha256']
                texts[key] = data.decode('utf8')
                entries.append(f'- `{item["path"]}`: [source text {key}](#text-{key})\n')
    if args.require_complete and unresolved:
        raise ValueError('Unresolved notice packages: ' + ', '.join(unresolved))
    header = (
        '# Crate source notice texts\n\n'
        'Generated from the pinned Cargo dependency inventory and checksummed source archives. '
        'Includes normal and build dependencies; inclusion is not proof of linked code. '
        'Identical source files share a text section. SHA-256 identifiers refer to original file bytes. '
        'Formatting removes trailing whitespace only. Repository supplements, when supplied, '
        'are verified against each archive’s recorded revision and the pinned source-text hash. Workspace notices verify local package identity, declared license and source-text hashes.\n\n'
        'This is a partial notice collection. It supplements LICENSE.md and THIRD_PARTY_NOTICES.md. '
        'Inline attributions, unconventional filenames and other bundled components '
        'require further review before a complete distribution-notice claim.\n\n'
        f'Target: `{inventory["target"]}`. Cargo.lock SHA-256: `{inventory["lockfileSha256"]}`.\n\n'
        '## Packages without collected archive or supplemental texts\n\n' + ', '.join(unresolved) + '\n\n'
        '## Source file index\n'
    )
    sections = []
    for key, text in sorted(texts.items()):
        sections.append(f'\n<a id="text-{key}"></a>\n\n## Source text {key}\n\n'
                        + '\n'.join(line.rstrip() for line in text.splitlines()) + '\n')
    result = header + ''.join(entries) + ''.join(sections)
    if args.check:
        if args.output.read_text() != result:
            raise ValueError('Crate notice bundle is stale; regenerate it')
    else:
        args.output.write_text(result)
    print(f'Verified {len(texts)} distinct notice texts; {len(unresolved)} packages require separate notice review')


if __name__ == '__main__':
    main()
