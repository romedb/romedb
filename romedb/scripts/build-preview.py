#!/usr/bin/env python3
"""Build a retained Linux x64 evaluation bundle from a clean pinned checkout."""
import hashlib
import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess

root = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('output', type=Path)
parser.add_argument('--release', action='store_true', help='Build optimized artifacts; does not publish or certify stable V1')
parser.add_argument('--label', default='0.1.0-linux-x64', help='Candidate identity recorded in the manifest')
args = parser.parse_args()
out = args.output.resolve()
def run(args, **kwargs):
    return subprocess.check_output(args, cwd=root, text=True, **kwargs)
if platform.system() != 'Linux' or platform.machine() != 'x86_64':
    raise SystemExit('This preview builder supports Linux x64 only')
if run(['git', 'status', '--porcelain']).strip():
    raise SystemExit('Commit tracked changes before building a candidate')
source = run(['git', 'rev-parse', 'HEAD']).strip()
out.mkdir(parents=True, exist_ok=False)
(out/'evidence').mkdir()
profile = 'release' if args.release else 'debug'
run(['cargo', 'build', '--locked', '-p', 'romedb-cli', '-p', 'romedb-node'] + (['--release'] if args.release else []))
shutil.copy2(root/f'target/{profile}/romedb-cli', out/'romedb-cli')
shutil.copy2(root/f'target/{profile}/libromedb_node.so', root/'romedb/bindings/node/romedb.node')
packed = json.loads(subprocess.check_output(
    ['pnpm', 'pack', '--json', '--pack-destination', str(out)],
    cwd=root/'romedb/bindings/node', text=True))
node_package = (out / packed['filename']).resolve()
if node_package.parent != out or not node_package.is_file():
    raise SystemExit('pnpm did not produce a package in the candidate directory')
(out/'tracker.cjs').write_text((root/'romedb/examples/node-task-tracker/app.cjs').read_text().replace("require('../../bindings/node/index.cjs')", "require('@romedb/node')"))

run(['git', 'archive', '--format=tar.gz', '--prefix=romedb-source/', '-o', str(out/'romedb-source.tar.gz'), source])
for src, dst in [('romedb/docs/preview-quickstart.md','README.md'), ('romedb/docs/preview-release.md','LIMITATIONS.md'), ('romedb/UPSTREAM.md','UPSTREAM.md'), ('romedb/bindings/node/LICENSE.md','LICENSE.md'), ('romedb/bindings/node/THIRD_PARTY_NOTICES.md','THIRD_PARTY_NOTICES.md'), ('romedb/bindings/node/THIRD_PARTY_CRATE_NOTICES.md','NODE_CRATE_NOTICES.md')]:
    shutil.copy2(root/src, out/dst)
if args.release:
    for src, dst in [('v1-candidate-quickstart.md','README.md'), ('v1-release-contract.md','LIMITATIONS.md'), ('node-sdk.md','SDK.md'), ('v1-query-matrix.md','QUERY-CONTRACT.md'), ('backup-restore.md','BACKUP.md'), ('v1-million-vector-evidence.md','PERFORMANCE.md')]:
        shutil.copy2(root/'romedb/docs'/src, out/dst)
for package in ['romedb-cli', 'romedb-node']:
    inventory = out/'evidence'/f'{package}-dependencies.json'
    audit = out/'evidence'/f'{package}-notice-audit.json'
    env = dict(os.environ, ROMEDB_INVENTORY_PACKAGE=package)
    run(['node','romedb/scripts/inventory-node-dependencies.cjs','x86_64-unknown-linux-gnu',str(inventory)], env=env)
    run(['python3','romedb/scripts/audit-crate-notices.py',str(inventory),str(audit)])
    run(['python3','romedb/scripts/bundle-crate-notices.py',str(inventory),str(audit),str(out/f'{package}-CRATE_NOTICES.md'),'--supplements','romedb/docs/notice-source-supplements.json'])
manifest = {'preview': args.label, 'sourceCommit': source, 'buildProfile': profile, 'platform': platform.platform(), 'rust': run(['rustc','-vV']), 'node': run(['node','--version']).strip(), 'packageManager': run(['pnpm','--version']).strip(), 'nodePackage': node_package.name, 'publication': 'local candidate only'}
(out/'manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')
files = sorted(p for p in out.rglob('*') if p.is_file())
(out/'SHA256SUMS').write_text(''.join(hashlib.sha256(p.read_bytes()).hexdigest()+'  '+str(p.relative_to(out))+'\n' for p in files))
print(out)
