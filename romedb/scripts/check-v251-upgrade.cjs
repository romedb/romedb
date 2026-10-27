'use strict';
// Open separate copies with immutable old/new packages, never two owning processes.
// RomeDB 0.x patches change no stored format, so an older package must keep reading
// files that a newer one wrote.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { execFileSync } = require('node:child_process');
const [oldPackage, newPackage, directory, phase, file] = process.argv.slice(2);
const oldVersion = require(path.join(oldPackage, 'package.json')).version;
const newVersion = require(path.join(newPackage, 'package.json')).version;
const versions = require('./versions.cjs');
versions.assertCandidate(oldVersion, '0.2.9');
versions.assertCandidate(newVersion, '0.2.10');
assert.ok(versions.compare(oldVersion, newVersion) < 0, `${oldVersion} -> ${newVersion}`);
const digest = file => createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const point = (x, y) => ({ type: 'Point', coordinates: [x, y] });
if (!phase) {
  fs.mkdirSync(directory);
  const source = path.join(directory, 'source.db');
  const backup = path.join(directory, 'previous-backup.db');
  const run = (step, database) => execFileSync(process.execPath,
    [__filename, oldPackage, newPackage, directory, step, database], { stdio: 'inherit', timeout: 120000 });
  run('seed', source);
  fs.copyFileSync(source, backup, fs.constants.COPYFILE_EXCL);
  const originalHash = digest(backup);
  run('upgrade', source); run('old-reader', source); run('reopen', source);
  const restored = path.join(directory, 'restored.db');
  fs.copyFileSync(backup, restored, fs.constants.COPYFILE_EXCL);
  run('upgrade', restored); run('old-reader', restored); run('reopen', restored);
  assert.equal(digest(backup), originalHash);
  const report = { fromVersion: oldVersion, toVersion: newVersion, node: process.version,
    fromAddonSha256: digest(path.join(oldPackage, 'romedb.node')),
    toAddonSha256: digest(path.join(newPackage, 'romedb.node')),
    previousBackupSha256: originalHash, backupsUnchanged: true,
    indexedResultsMatchScans: true, previousReaderOpensUpgradedFile: true,
    reopen: true, independentRestore: true };
  fs.writeFileSync(path.join(directory, 'report.json'), JSON.stringify(report, null, 2) + '\n');
} else {
  const { Database, Record } = require(phase === 'seed' || phase === 'old-reader' ? oldPackage : newPackage);
  const db = new Database(file);
  const ids = rows => rows.map(row => String(row[0].key));
  const matchesScan = sql => {
    const indexed = db.all(sql);
    assert.deepEqual(indexed, db.all(sql.replace(/FROM (places|zones)\b/, 'FROM $1 NOT INDEXED')), sql);
    return ids(indexed);
  };
  const queries = () => ({
    near: matchesScan('SELECT id FROM places WHERE ST_DWithin(location, ST_Point(106.7, 10.8), 3000) ORDER BY id'),
    nearest: matchesScan("SELECT id FROM places WHERE ST_DistanceSphere(location, ST_Point(106.7, 10.8)) IS NOT NULL AND fleet = 'night' ORDER BY ST_DistanceSphere(location, ST_Point(106.7, 10.8)), id LIMIT 3"),
    zones: matchesScan('SELECT id FROM zones WHERE ST_Covers(shape, ST_Point(106.75, 10.85)) ORDER BY id'),
    viewport: ids(db.all("SELECT id FROM search::bbox('places_location', 106.69, 10.79, 106.75, 10.83) ORDER BY id")),
    fleet: db.all("SELECT name FROM places WHERE fleet = 'night' ORDER BY id").map(row => row[0]),
  });
  try {
    if (phase === 'seed') {
      db.execute('CREATE TABLE places');
      db.execute('CREATE TABLE zones');
      db.execute('BEGIN');
      for (let i = 0; i < 60; i++) {
        db.execute('INSERT INTO places DOCUMENT $d', { $d: { id: new Record('places', BigInt(i)), name: `place ${i}`,
          fleet: i % 10 === 0 ? 'night' : 'day', location: point(106.7 + (i % 8) * 0.01, 10.8 + Math.floor(i / 8) * 0.01) } });
      }
      db.execute('INSERT INTO zones {id:zones:north,shape:ST_MakeEnvelope(106.69, 10.82, 106.8, 10.9)}');
      db.execute('INSERT INTO zones {id:zones:south,shape:ST_MakeEnvelope(106.69, 10.7, 106.8, 10.81)}');
      db.execute('COMMIT');
      db.execute('CREATE SEARCH INDEX places_location ON places(location) USING SPATIAL');
      db.execute('CREATE INDEX places_fleet ON places(fleet)');
      db.execute("CREATE SEARCH INDEX zones_shape ON zones(shape) USING SPATIAL WITH (geometry = 'region')");
      const seeded = queries();
      assert.deepEqual(seeded.zones, ['north']);
      assert.deepEqual(seeded.nearest, ['0', '10', '20']);
    } else {
      const before = queries();
      assert.ok(before.near.length >= 5 && before.viewport.length >= 5);
      if (phase === 'upgrade') {
        // Writes from the newer package that the older one must read back.
        db.execute('UPDATE places SET location = geo::point(106.75, 10.85), name = $n WHERE id = places:1', { $n: 'moved' });
        db.execute("UPSERT places:60 {name:'new night', fleet:'night', location:geo::point(106.7001, 10.8001)}");
        db.execute('DELETE FROM places WHERE id = places:2');
        db.execute('INSERT INTO zones {id:zones:center,shape:ST_MakeEnvelope(106.74, 10.84, 106.76, 10.86)}');
      }
      const after = queries();
      assert.deepEqual(after.zones, ['center', 'north']);
      assert.deepEqual(after.nearest, ['0', '60', '10']);
      assert.deepEqual(db.all('SELECT name FROM places WHERE id = places:1'), [['moved']]);
      assert.equal(db.checkCollectionIntegrity('places').documents, 60n);
      assert.equal(db.checkCollectionIntegrity('zones').documents, 3n);
    }
  } finally {
    // Produce a self-contained offline backup before copying the main file.
    assert.deepEqual(db.all('PRAGMA wal_checkpoint(TRUNCATE)'), [[0n, 0n, 0n]]);
    db.close();
  }
}
