#!/usr/bin/env python3
"""Synthetic offline checks for notice-bundle integrity and failure atomicity."""
import contextlib
import hashlib
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import types
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name('bundle-crate-notices.py')


class NoticeBundleTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.module = types.ModuleType('notice_bundle_test_module')
        self.module.__file__ = str(SCRIPT)
        exec(compile(SCRIPT.read_text(), str(SCRIPT), 'exec'), self.module.__dict__)
        self.module.ROOT = self.root
        self.cache = self.root / 'cargo/registry/cache/test-registry'
        self.cache.mkdir(parents=True)
        self.archive = self.cache / 'fixture-1.0.0.crate'
        self.text = b'Fixture copyright and notice text.\n'
        files = {'LICENSE': self.text, 'nested/LICENSE.txt': self.text}
        archive_files = dict(files, **{'Cargo.toml': b'[package]\nname="fixture"\nversion="1.0.0"\nlicense="MIT OR Apache-2.0"\n', '.cargo_vcs_info.json': json.dumps({
            'git': {'sha1': 'a' * 40}, 'path_in_vcs': 'crates/fixture',
        }).encode()})
        with tarfile.open(self.archive, 'w:gz') as archive:
            for name, data in archive_files.items():
                member = tarfile.TarInfo('fixture-1.0.0/' + name)
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
        checksum = self.hash(self.archive.read_bytes())
        lock = f'[[package]]\nname = "fixture"\nversion = "1.0.0"\nsource = "registry+fixture"\nchecksum = "{checksum}"\n'
        (self.root / 'Cargo.lock').write_text(lock)
        self.inventory = self.root / 'inventory.json'
        self.inventory.write_text(json.dumps({
            'target': 'fixture-target', 'lockfileSha256': self.hash(lock.encode()),
            'packages': [{'name': 'fixture', 'version': '1.0.0'}],
        }))
        self.report = {
            'inventorySha256': self.hash(self.inventory.read_bytes()),
            'packages': [{
                'name': 'fixture', 'version': '1.0.0', 'archiveSha256': checksum,
                'noticeCandidates': [{'path': name, 'bytes': len(data), 'sha256': self.hash(data)} for name, data in files.items()],
            }],
        }
        self.audit = self.root / 'audit.json'
        self.output = self.root / 'bundle.md'
        self.output.write_text('existing output')
        self.supplements = None

    @staticmethod
    def hash(data):
        return hashlib.sha256(data).hexdigest()

    def run_bundle(self, check=False, require_complete=False):
        self.audit.write_text(json.dumps(self.report))
        args = [str(SCRIPT), str(self.inventory), str(self.audit), str(self.output)]
        if check:
            args.append('--check')
        if require_complete:
            args.append('--require-complete')
        if self.supplements:
            args.extend(['--supplements', str(self.supplements)])
        with patch.object(sys, 'argv', args), patch.dict('os.environ', {'CARGO_HOME': str(self.root / 'cargo')}), contextlib.redirect_stdout(io.StringIO()):
            self.module.main()

    def reject(self, message, check=False):
        with self.assertRaisesRegex(ValueError, message):
            self.run_bundle(check)
        self.assertEqual(self.output.read_text(), 'existing output')

    def test_reproducible_bundle_deduplicates_text_and_keeps_each_source(self):
        self.run_bundle()
        first = self.output.read_bytes()
        self.run_bundle(check=True)
        self.run_bundle()
        self.assertEqual(self.output.read_bytes(), first)
        text = first.decode()
        self.assertEqual(text.count(self.text.decode()), 1)
        self.assertIn('`LICENSE`', text)
        self.assertIn('`nested/LICENSE.txt`', text)

    def add_supplement(self):
        self.supplements = self.root / 'supplements.json'
        source = self.root / 'supplement.txt'
        source.write_bytes(self.text)
        entry = {
            'package': 'fixture', 'version': '1.0.0',
            'sourceRevision': 'a' * 40, 'cratePathInSource': 'crates/fixture',
            'sourceUrl': 'https://example.test/' + 'a' * 40 + '/LICENSE',
            'sourceSha256': self.hash(self.text), 'sourceFile': source.name,
        }
        self.supplements.write_text(json.dumps({'schemaVersion': 1, 'entries': [entry]}))
        return entry

    def test_supplement_collects_missing_notice_and_deduplicates(self):
        self.add_supplement()
        self.run_bundle()
        text = self.output.read_text()
        self.assertEqual(text.count(self.text.decode()), 1)
        self.assertIn('Repository source', text)
        self.report['packages'][0]['noticeCandidates'] = []
        self.run_bundle()
        self.run_bundle(check=True)
        text = self.output.read_text()
        unresolved = text.split('## Packages without')[1].split('## Source file index')[0]
        self.assertNotIn('fixture 1.0.0', unresolved)
        self.assertIn(self.text.decode(), text)

    def test_altered_supplement_preserves_output(self):
        self.add_supplement()
        (self.root / 'supplement.txt').write_text('altered')
        self.reject('Supplement checksum mismatch')

    def test_wrong_revision_supplement_preserves_output(self):
        entry = self.add_supplement()
        entry['sourceRevision'] = 'b' * 40
        self.supplements.write_text(json.dumps({'schemaVersion': 1, 'entries': [entry]}))
        self.reject('Supplement revision differs')

    def test_supplement_requires_valid_archive_even_without_archive_notice(self):
        self.add_supplement()
        self.report['packages'][0]['noticeCandidates'] = []
        self.archive.write_bytes(self.archive.read_bytes() + b'altered')
        self.reject('Archive checksum mismatch')

    def add_declaration(self):
        self.supplements = self.root / 'supplements.json'
        (self.root / 'reference.txt').write_bytes(self.text)
        entry = {
            'package': 'fixture', 'version': '1.0.0',
            'archiveSha256': self.report['packages'][0]['archiveSha256'],
            'licenseExpression': 'MIT OR Apache-2.0', 'selectedLicense': 'MIT',
            'textSources': [{'licenseId': 'MIT', 'sourceUrl': 'https://example.test/MIT',
                             'sourceFile': 'reference.txt', 'sourceSha256': self.hash(self.text)}],
        }
        self.supplements.write_text(json.dumps({'schemaVersion': 1, 'entries': [], 'declarationEntries': [entry]}))
        self.report['packages'][0]['noticeCandidates'] = []
        return entry

    def write_declaration(self, entry):
        self.supplements.write_text(json.dumps({'schemaVersion': 1, 'entries': [], 'declarationEntries': [entry]}))

    def test_declaration_retains_manifest_and_reference_without_inventing_notice(self):
        self.add_declaration()
        self.run_bundle()
        self.run_bundle(check=True)
        text = self.output.read_text()
        self.assertIn('distribution selects `MIT`', text)
        self.assertIn('not a fabricated upstream LICENSE file', text)
        self.assertIn('license="MIT OR Apache-2.0"', text)
        self.assertNotIn('fixture 1.0.0', text.split('## Packages without')[1].split('## Source file index')[0])

    def test_declaration_wrong_selection_preserves_output(self):
        entry = self.add_declaration()
        entry['selectedLicense'] = 'BSD-3-Clause'
        self.write_declaration(entry)
        self.reject('Declaration license selection mismatch')

    def test_declaration_changed_reference_preserves_output(self):
        self.add_declaration()
        (self.root / 'reference.txt').write_text('changed')
        self.reject('Declaration text checksum mismatch')

    def test_declaration_wrong_archive_preserves_output(self):
        entry = self.add_declaration()
        entry['archiveSha256'] = '0' * 64
        self.write_declaration(entry)
        self.reject('Declaration archive mismatch')

    def test_declaration_requires_selected_license_text(self):
        entry = self.add_declaration()
        entry['textSources'] = []
        self.write_declaration(entry)
        self.reject('Declaration lacks selected license text')

    def test_complete_mode_rejects_uncollected_package_without_replacing_output(self):
        self.report['packages'][0]['noticeCandidates'] = []
        with self.assertRaisesRegex(ValueError, 'Unresolved notice packages'):
            self.run_bundle(require_complete=True)
        self.assertEqual(self.output.read_text(), 'existing output')

    def test_complete_mode_accepts_verified_declaration(self):
        self.add_declaration()
        self.run_bundle(require_complete=True)

    def add_workspace(self):
        lock = '[[package]]\nname = "fixture"\nversion = "1.0.0"\n'
        (self.root / 'Cargo.lock').write_text(lock)
        inventory = json.loads(self.inventory.read_text())
        inventory['lockfileSha256'] = self.hash(lock.encode())
        self.inventory.write_text(json.dumps(inventory))
        self.report['inventorySha256'] = self.hash(self.inventory.read_bytes())
        (self.root / 'Cargo.toml').write_text('[workspace.package]\nversion="1.0.0"\nlicense="MIT"\n')
        (self.root / 'fixture').mkdir()
        (self.root / 'fixture/Cargo.toml').write_text('[package]\nname="fixture"\nversion.workspace=true\nlicense.workspace=true\n')
        (self.root / 'LICENSE').write_bytes(self.text)
        self.supplements = self.root / 'supplements.json'
        self.supplements.write_text(json.dumps({'schemaVersion': 1, 'entries': [], 'workspaceEntries': [{
            'package': 'fixture', 'version': '1.0.0', 'manifestPath': 'fixture/Cargo.toml',
            'licenseExpression': 'MIT', 'noticeFiles': [{'path': 'LICENSE', 'sha256': self.hash(self.text)}],
        }]}))
        self.report['packages'][0] = {'name': 'fixture', 'version': '1.0.0', 'status': 'workspace_source_requires_separate_review'}

    def test_workspace_notice_checks_inherited_manifest_and_text(self):
        self.add_workspace()
        self.run_bundle()
        self.run_bundle(check=True)
        self.assertIn('Workspace `LICENSE`', self.output.read_text())
        self.assertIn(self.text.decode(), self.output.read_text())
        self.assertNotIn('fixture 1.0.0', self.output.read_text().split('## Packages without')[1].split('## Source file index')[0])

    def test_workspace_license_change_preserves_output(self):
        self.add_workspace()
        (self.root / 'Cargo.toml').write_text('[workspace.package]\nversion="1.0.0"\nlicense="Apache-2.0"\n')
        self.reject('Workspace package identity or license changed')

    def test_workspace_corrupt_notice_preserves_output(self):
        self.add_workspace()
        (self.root / 'LICENSE').write_bytes(b'changed')
        self.reject('Workspace notice checksum mismatch')

    def test_stale_inventory_preserves_output(self):
        (self.root / 'Cargo.lock').write_text('changed lockfile')
        self.reject('Stale inventory')

    def test_mismatched_package_identity_preserves_output(self):
        self.report['packages'][0]['version'] = '2.0.0'
        self.reject('package identities differ')

    def test_corrupted_archive_preserves_output(self):
        self.archive.write_bytes(self.archive.read_bytes() + b'altered')
        self.reject('Archive checksum mismatch')

    def test_altered_notice_hash_preserves_output(self):
        self.report['packages'][0]['noticeCandidates'][0]['sha256'] = '0' * 64
        self.reject('Notice checksum mismatch')

    def test_stale_bundle_check_preserves_output(self):
        self.reject('bundle is stale', check=True)


if __name__ == '__main__':
    unittest.main()
