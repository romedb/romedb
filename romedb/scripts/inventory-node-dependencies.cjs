'use strict';
// Dependency declaration inventory, including build dependencies. This is not
// a linked-binary inventory or a complete third-party notice bundle.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');
const [target, output, mode] = process.argv.slice(2);
if (!target || !output || (mode !== undefined && mode !== '--check') || process.argv.length > 6) {
  throw new Error('Usage: node inventory-node-dependencies.cjs <target-triple> <output.json> [--check]');
}
const rootPackage = process.env.ROMEDB_INVENTORY_PACKAGE || 'romedb-node';
const root = path.resolve(__dirname, '../..');
const args = ['tree', '--locked', '--offline', '-p', rootPackage, '--edges', 'normal,build', '--target', target, '--prefix', 'none', '--format', '{p}\t{l}'];
const tree = execFileSync('cargo', args, { cwd: root, encoding: 'utf8', maxBuffer: 8 * 1024 * 1024, timeout: 120000 });
const packages = new Map();
for (const line of tree.trimEnd().split('\n')) {
  const match = /^(\S+) v(\S+)[^\t]*\t(.*)$/.exec(line);
  if (!match) throw new Error(`Unexpected cargo tree output: ${line}`);
  const [, name, version, rawLicense] = match;
  const licenseExpression = rawLicense.replace(/ \(\*\)$/, '').trim() || null;
  const key = `${name}@${version}`;
  const previous = packages.get(key);
  if (previous && previous.licenseExpression !== licenseExpression) {
    throw new Error(`Conflicting license declarations for ${key}`);
  }
  packages.set(key, { name, version, licenseExpression });
}
const inventory = {
  schemaVersion: 1,
  rootPackage,
  target,
  edges: ['normal', 'build'],
  scope: 'Cargo dependency declarations; includes build tools, excludes dev dependencies; not proof of linked code or complete notices',
  lockfileSha256: crypto.createHash('sha256').update(fs.readFileSync(path.join(root, 'Cargo.lock'))).digest('hex'),
  packages: [...packages.values()].sort((a, b) => {
    const left = `${a.name}@${a.version}`;
    const right = `${b.name}@${b.version}`;
    return left < right ? -1 : left > right ? 1 : 0;
  }),
};
const serialized = JSON.stringify(inventory, null, 2) + '\n';
if (mode === '--check') {
  if (fs.readFileSync(output, 'utf8') !== serialized) throw new Error('Dependency inventory is stale; regenerate it for this target');
} else {
  fs.writeFileSync(output, serialized);
}
console.log(`${mode === '--check' ? 'Verified' : 'Wrote'} ${packages.size} dependency declarations for ${target}`);
