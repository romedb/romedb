'use strict';
// The file API of `romedb serve` with the client that applications use: `db.files` for a
// backend and the uploader of a page. Run: node files-sdk.cjs [path to the romedb binary].
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const { randomUUID, randomBytes } = require('node:crypto');
const { spawn } = require('node:child_process');
const { connect, ClientError } = require('../../bindings/client/client.cjs');
const { createUploader, crc64nvme } = require('../../bindings/client/uploads.cjs');
const { freePort, start, stop } = require('./sdk.cjs');

const MIB = 1024 * 1024;

async function startDirectory(binary, directory, port) {
  const child = spawn(binary, ['serve', '--directory', directory, '--create', '--port', String(port)], { stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stderr.on('data', chunk => { stderr += chunk; });
  try {
    await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error('service startup timed out')), 15000);
      child.stdout.on('data', chunk => {
        stdout += chunk;
        if (stdout.split('\n').length >= 3) { clearTimeout(timeout); resolve(); }
      });
      child.once('exit', () => { clearTimeout(timeout); reject(new Error(`service startup failed: ${stderr}`)); });
    });
    const organization = /^Organization URL: (\S+)$/m.exec(stdout)[1];
    const token = (await fs.readFile(path.join(directory, 'romedb.token'), 'utf8')).trim();
    return { child, token, organization };
  } catch (error) { child.kill('SIGKILL'); throw error; }
}

async function download(url) {
  const response = await fetch(url);
  assert.equal(response.status, 200);
  return { response, bytes: Buffer.from(await response.arrayBuffer()) };
}

async function names(folder) {
  return (await fs.readdir(folder).catch(() => [])).sort();
}

// Every call an application makes, against one database URL.
async function exercise(db, filesFolder, organizationId) {
  const bytes = randomBytes(20 * MIB);
  const steps = [];
  const file = await db.files.upload(new Blob([bytes]), {
    name: 'Team photo ü.png', mediaType: 'image/png', subject: 'user-1', metadata: { album: 'offsite', year: 2026 },
    onProgress: step => steps.push(step),
  });
  assert.match(file.id, /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  assert.equal(file.databaseId, db.databaseId);
  assert.deepEqual(
    { status: file.status, name: file.name, description: file.description, tags: file.tags, metadata: file.metadata, size: file.size, mediaType: file.mediaType, createdBy: file.createdBy },
    { status: 'ready', name: 'Team photo ü.png', description: '', tags: [], metadata: { album: 'offsite', year: 2026 }, size: bytes.length, mediaType: 'image/png', createdBy: 'user-1' });
  assert.deepEqual(file.checksum, { algorithm: 'crc64nvme', value: crc64nvme().update(bytes).base64() });
  assert.deepEqual(steps.at(-1), { uploadedBytes: bytes.length, totalBytes: bytes.length, uploadedParts: 3, totalParts: 3 });
  assert.deepEqual(await names(path.join(filesFolder, 'objects')), [file.id]);
  assert.deepEqual(await names(path.join(filesFolder, 'uploads')), []);
  assert.equal((await fs.stat(path.join(filesFolder, 'objects', file.id))).size, bytes.length);

  const link = await db.files.createDownloadUrl(file.id);
  assert.ok(link.url.startsWith(`${db.url}/files/${file.id}/content?`));
  assert.ok(link.expiresAt > Date.now() && link.expiresAt <= Date.now() + 300_000);
  const attached = await download(link.url);
  assert.ok(attached.bytes.equals(bytes));
  assert.equal(attached.response.headers.get('content-type'), 'image/png');
  assert.equal(attached.response.headers.get('content-length'), String(bytes.length));
  assert.equal(attached.response.headers.get('content-disposition'), `attachment; filename="Team photo _.png"; filename*=UTF-8''Team%20photo%20%C3%BC.png`);
  const shown = await download((await db.files.createDownloadUrl(file.id, { disposition: 'inline', fileName: 'photo.png', expiresInSeconds: 60 })).url);
  assert.equal(shown.response.headers.get('content-disposition'), `inline; filename="photo.png"; filename*=UTF-8''photo.png`);
  assert.ok(shown.bytes.equals(bytes));

  assert.deepEqual(await db.files.get(file.id), file);
  assert.deepEqual(await db.files.list(), { files: [file], next: null });
  const row = (await db.read([{ sql: 'SELECT name, size, mediaType, status, createdBy FROM romedb_files' }])).results[0].rows;
  assert.deepEqual(row, [['Team photo ü.png', BigInt(bytes.length), 'image/png', 'ready', 'user-1']]);

  // A page uploads with a session that the backend opened; it never has the service token.
  const uploader = createUploader({ authorize: intent => db.files.createUploadSession({ id: intent.id, name: intent.name, size: intent.size, mediaType: intent.type, subject: 'user-2' }) });
  let session;
  const note = await uploader.upload(new Blob(['a note from the page'], { type: 'text/plain' }), { name: 'note.txt', onSession: opened => { session = opened; } });
  assert.match(session.token, /^rdbu_[A-Za-z0-9_-]{43}$/);
  assert.equal(session.url, `${db.url}/files/uploads/${session.id}`);
  assert.equal((await uploader.status(session)).state, 'ready');
  assert.equal((await db.files.uploadStatus(session.id)).file.id, note.id);
  assert.deepEqual(await db.files.completeUpload(session.id), note);
  await assert.rejects(db.files.abortUpload(session.id), error => error instanceof ClientError && error.code === 'RDB_CONFLICT');
  assert.equal(note.createdBy, 'user-2');
  assert.ok(note.id > file.id);

  const first = await db.files.list({ limit: 1 });
  assert.deepEqual(first, { files: [file], next: file.id });
  assert.deepEqual(await db.files.list({ after: first.next }), { files: [note], next: null });
  assert.deepEqual((await db.files.list({ order: 'desc' })).files.map(listed => listed.id), [note.id, file.id]);

  const changed = await db.files.update(file.id, { name: 'team.png', description: 'The whole team', tags: ['photo', 'team'], metadata: { album: 'offsite' } });
  assert.deepEqual(
    { ...file, name: 'team.png', description: 'The whole team', tags: ['photo', 'team'], metadata: { album: 'offsite' }, updatedAt: changed.updatedAt }, changed);
  assert.ok(changed.updatedAt >= file.updatedAt);
  await assert.rejects(db.files.update(file.id, { name: '' }), error => error.code === 'RDB_REMOTE' && error.status === 400 && error.message === 'File name');

  const usage = await db.files.usage();
  assert.deepEqual(usage, {
    organizationId, plan: 'local', bytes: bytes.length + 20, reservedBytes: 0, files: 2, openUploads: 0, peakBytes: bytes.length + 20,
    allowanceBytes: Number.MAX_SAFE_INTEGER, charge: { blocks: 0, blockBytes: 100_000_000_000, usdPerBlockMonth: 0, billedBytes: 0, monthlyUsd: 0 },
  });

  const several = await db.files.createDownloadUrls([note.id, file.id, randomUUID()], { disposition: 'inline', expiresInSeconds: 60 });
  assert.deepEqual(several.downloads.map(entry => entry.fileId), [note.id, file.id]);
  assert.ok((await download(several.downloads[0].url)).bytes.equals(Buffer.from('a note from the page')));
  assert.ok((await download(several.downloads[1].url)).bytes.equals(bytes));

  await assert.rejects(db.files.process(file.id, { kind: 'image', recipe: { width: 320 } }),
    error => error instanceof ClientError && error.status === 503 && /File processing is unavailable/.test(error.message));
  await assert.rejects(db.files.derivatives(file.id), /File processing is unavailable/);

  const deletion = await db.files.delete(file.id);
  assert.deepEqual({ fileId: deletion.fileId, databaseId: deletion.databaseId, state: deletion.state }, { fileId: file.id, databaseId: db.databaseId, state: 'done' });
  assert.deepEqual(await db.files.deletionStatus(file.id), deletion);
  await assert.rejects(db.files.get(file.id), error => error.code === 'RDB_NOT_FOUND');
  assert.equal((await fetch(link.url)).status, 404);
  assert.deepEqual(await names(path.join(filesFolder, 'objects')), [note.id]);

  // Deleting the row with SQL is the request to delete the file; the next list removes the bytes.
  await db.writeAtomic([{ sql: `DELETE romedb_files:\`${note.id}\`` }]);
  assert.deepEqual(await db.files.list(), { files: [], next: null });
  assert.deepEqual(await names(path.join(filesFolder, 'objects')), []);
  assert.equal((await db.files.usage()).bytes, 0);
}

async function main() {
  const binary = process.argv[2] ? path.resolve(process.argv[2]) : path.resolve(__dirname, '../../../target/debug/romedb');
  const fixtureRoot = path.resolve(__dirname, '../../../../.local/tmp');
  await fs.mkdir(fixtureRoot, { recursive: true });
  const directory = await fs.mkdtemp(path.join(fixtureRoot, 'service-files-sdk-'));
  let service, db;
  try {
    const file = path.join(directory, 'app.romedb');
    service = await start(binary, file, await freePort(), true);
    db = await connect(service);
    await exercise(db, `${file}.files`, db.databaseId);

    // Upload sessions and signed URLs end with the process; stored files stay.
    const kept = await db.files.upload(Buffer.from('kept over a restart'), { name: 'kept.txt', mediaType: 'text/plain' });
    const open = await db.files.createUploadSession({ name: 'open.bin', size: 5 });
    const before = await db.files.createDownloadUrl(kept.id);
    await db.close(); db = undefined;
    const port = Number(new URL(service.url).port);
    await stop(service.child);
    service = await start(binary, file, port, false);
    db = await connect(service);
    assert.deepEqual(await db.files.list(), { files: [kept], next: null });
    assert.equal((await fetch(before.url)).status, 403);
    assert.ok((await download((await db.files.createDownloadUrl(kept.id)).url)).bytes.equals(Buffer.from('kept over a restart')));
    await assert.rejects(db.files.uploadStatus(open.id), error => error.code === 'RDB_NOT_FOUND');
    assert.deepEqual(await names(path.join(`${file}.files`, 'uploads')), []);
    await db.close(); db = undefined;
    await stop(service.child); service = undefined;

    const root = path.join(directory, 'data');
    service = await startDirectory(binary, root, await freePort());
    const created = await fetch(`${service.organization}/databases`, {
      method: 'POST', headers: { authorization: `Bearer ${service.token}`, 'content-type': 'application/json' }, body: JSON.stringify({ name: 'app' }),
    });
    assert.equal(created.status, 201);
    const { id } = await created.json();
    db = await connect({ url: `${service.organization}/databases/${id}`, token: service.token });
    assert.equal(db.cloud, false);
    await exercise(db, path.join(root, `${id}.files`), service.organization.split('/').at(-1));
    process.stdout.write('Service files SDK qualification passed: 20 MiB in three parts, page upload with a session, download, list, update, usage, several download URLs, delete, SQL delete, restart and directory mode.\n');
  } finally {
    if (db) await db.close();
    if (service) await stop(service.child);
    await fs.rm(directory, { recursive: true, force: true });
  }
}

main().catch(error => { process.stderr.write(`${error.stack}\n`); process.exitCode = 1; });
