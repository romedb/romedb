'use strict';
const assert = require('node:assert/strict');
const { test } = require('node:test');
const { connect, ClientError, createUploader, uploadToSession, UploadError } = require('./client.cjs');
const { crc64nvme } = require('./uploads.cjs');

const org = '11111111-1111-4111-8111-111111111111', database = '22222222-2222-4222-8222-222222222222';
const cloud = `https://cloud.romedb.com/v1/organizations/${org}/databases/${database}`;
const json = (body, status = 200) => Response.json(body, { status });
const bytes = (size, seed = 1) => { const out = new Uint8Array(size); for (let at = 0; at < size; at++) out[at] = (at * 31 + seed) & 0xff; return out; };
const checksum = data => crc64nvme().update(data).base64();

// RomeDB Cloud and its storage, as far as an upload sees them: the session routes, one
// URL per part, and a file published from the parts that arrived.
function service({ size, partSize = 1000, state = 'uploading' } = {}) {
  const session = {
    id: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', fileId: '01939b2e-7c1a-7000-8000-000000000001', databaseId: database, organizationId: org, state,
    name: 'report.pdf', size, mediaType: 'application/pdf', partSize, parts: Math.ceil(size / partSize), createdAt: 1, expiresAt: 2,
    url: `${cloud}/files/uploads/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa`, token: `rdbu_${'t'.repeat(43)}`,
  };
  const world = { session, stored: new Map(), requests: [], puts: [], urlVersion: 0, file: null, hooks: {} };
  const partBytes = number => number === session.parts ? size - (session.parts - 1) * partSize : partSize;
  const status = () => {
    const { token, ...view } = session;
    return { ...view, uploadedParts: session.state === 'uploading' ? [...world.stored.keys()].sort((a, b) => a - b) : [], ...(session.state === 'ready' ? { file: world.file } : {}) };
  };
  world.fetch = async (input, init = {}) => {
    const url = String(input), method = init.method ?? 'GET';
    if (init.signal?.aborted) throw init.signal.reason;
    if (url.startsWith('https://storage.test/')) {
      const { pathname, searchParams } = new URL(url), number = Number(pathname.split('/').pop());
      const body = new Uint8Array(await new Response(init.body).arrayBuffer());
      world.puts.push({ number, headers: init.headers ?? {}, bytes: body.byteLength });
      const forced = await world.hooks.put?.(number, searchParams);
      if (forced) return forced;
      if (Number(searchParams.get('v')) < world.urlVersion) return new Response('expired', { status: 403 });
      if (body.byteLength !== partBytes(number)) return new Response('size', { status: 403 });
      world.stored.set(number, body);
      return new Response(null, { status: 200, headers: { etag: `"${number}"` } });
    }
    assert.ok(url.startsWith(session.url), `unexpected request to ${url}`);
    const path = url.slice(session.url.length);
    world.requests.push({ method, path, authorization: init.headers?.authorization, redirect: init.redirect, body: init.body });
    const forced = await world.hooks.call?.(method, path);
    if (forced) return forced;
    if (init.headers?.authorization !== `Bearer ${session.token}`) return json({ error: 'Upload session token is not valid for this upload' }, 401);
    if (method === 'GET' && path === '') return json(status());
    if (path === '/abort') { session.state = 'aborted'; return json(status()); }
    if (session.state !== 'uploading') return json({ error: `Upload is ${session.state}` }, 409);
    if (path === '/parts') {
      const { parts } = JSON.parse(init.body);
      return json({ parts: parts.map(partNumber => ({ partNumber, size: partBytes(partNumber), url: `https://storage.test/part/${partNumber}?v=${world.urlVersion}`, expiresAt: 3 })) });
    }
    if (path === '/complete') {
      const expected = JSON.parse(init.body ?? '{}');
      if (world.stored.size !== session.parts) return json({ error: 'Upload is incomplete' }, 409);
      const whole = new Uint8Array(size);
      for (const [number, part] of world.stored) whole.set(part, (number - 1) * partSize);
      if (expected.crc64nvme !== undefined && expected.crc64nvme !== checksum(whole)) { session.state = 'rejected'; session.reason = 'checksum'; return json({ error: 'File checksum differs' }, 409); }
      session.state = 'ready';
      world.file = { id: session.fileId, databaseId: database, status: 'ready', name: session.name, size, mediaType: session.mediaType, sentChecksum: expected.crc64nvme ?? null, bytes: [...whole] };
      return json({ file: world.file });
    }
    return json({ error: 'Not found' }, 404);
  };
  return world;
}

test('a page uploads a file in parts and gets the published file', async () => {
  const data = bytes(3500), world = service({ size: 3500 }), intents = [], progress = [], sessions = [];
  const uploader = createUploader({ authorize: async intent => { intents.push(intent); return world.session; }, fetch: world.fetch });
  const file = await uploader.upload(new File([data], 'report.pdf', { type: 'application/pdf' }),
    { context: { articleId: 7 }, onProgress: step => progress.push(step), onSession: session => sessions.push(session), concurrency: 2 });
  assert.deepEqual(new Uint8Array(file.bytes), data);
  assert.equal(file.sentChecksum, checksum(data));
  assert.equal(intents.length, 1);
  assert.match(intents[0].id, /^[0-9a-f-]{36}$/);
  assert.deepEqual({ ...intents[0], id: 0 }, { id: 0, name: 'report.pdf', size: 3500, type: 'application/pdf', context: { articleId: 7 } });
  assert.deepEqual(sessions, [world.session]);
  assert.deepEqual(world.puts.map(put => put.number).sort(), [1, 2, 3, 4]);
  assert.deepEqual(progress.at(-1), { uploadedBytes: 3500, totalBytes: 3500, uploadedParts: 4, totalParts: 4 });
  assert.ok(progress.every((step, index) => index === 0 || step.uploadedBytes >= progress[index - 1].uploadedBytes));
  // The session token goes to Cloud only, never to storage; and no redirect is followed with it.
  assert.ok(world.puts.every(put => !('authorization' in put.headers)));
  assert.ok(world.requests.every(request => request.authorization === `Bearer ${world.session.token}` && request.redirect === 'manual'));
});

test('an upload that stopped continues with the parts storage does not hold', async () => {
  const data = bytes(3500), world = service({ size: 3500 });
  world.stored.set(1, data.subarray(0, 1000)); world.stored.set(3, data.subarray(2000, 3000));
  const progress = [];
  const file = await uploadToSession(world.session, data, { fetch: world.fetch, onProgress: step => progress.push(step.uploadedParts) });
  assert.deepEqual(new Uint8Array(file.bytes), data);
  assert.deepEqual(world.puts.map(put => put.number).sort(), [2, 4]);
  assert.equal(progress[0], 2);
  const again = service({ size: 3500, state: 'ready' }); again.file = { id: 'done' };
  assert.deepEqual(await uploadToSession(again.session, data, { fetch: again.fetch }), { id: 'done' });
  assert.equal(again.puts.length, 0);
});

test('a part is sent again after a storage failure, a lost connection and an expired URL', async () => {
  const data = bytes(2500), world = service({ size: 2500 });
  const failures = { 1: [new Response('busy', { status: 503 })], 2: ['network'], 3: ['expire'] };
  world.hooks.put = async number => {
    const next = failures[number]?.shift();
    if (next === 'network') throw new TypeError('fetch failed');
    if (next === 'expire') { world.urlVersion += 1; return undefined; }
    return next;
  };
  const file = await uploadToSession(world.session, data, { fetch: world.fetch, concurrency: 1 });
  assert.deepEqual(new Uint8Array(file.bytes), data);
  assert.deepEqual(world.puts.map(put => put.number), [1, 1, 2, 2, 3, 3]);
  assert.deepEqual(world.requests.filter(request => request.path === '/parts').map(request => JSON.parse(request.body).parts), [[1, 2, 3], [3]]);
});

test('a part storage lost before completion is sent again', async () => {
  const data = bytes(2500), world = service({ size: 2500 });
  let lose = true;
  world.hooks.call = async (method, path) => { if (path === '/complete' && lose) { lose = false; world.stored.delete(2); } };
  const file = await uploadToSession(world.session, data, { fetch: world.fetch });
  assert.deepEqual(new Uint8Array(file.bytes), data);
  assert.deepEqual(world.puts.map(put => put.number).sort(), [1, 2, 2, 3]);
});

test('bytes that differ from the file are rejected, and that upload cannot continue', async () => {
  const data = bytes(1500), world = service({ size: 1500 });
  // Storage keeps other bytes for part 2 than the page sent.
  const corrupt = target => async number => { if (number === 2) { target.stored.set(2, bytes(500, 9)); return new Response(null, { status: 200 }); } };
  world.hooks.put = corrupt(world);
  await assert.rejects(uploadToSession(world.session, data, { fetch: world.fetch }), error =>
    error instanceof UploadError && error.code === 'UPLOAD_REJECTED' && error.recoverable === false && error.session === world.session && !JSON.stringify(error).includes('rdbu_'));
  const unchecked = service({ size: 1500 });
  unchecked.hooks.put = corrupt(unchecked);
  const file = await uploadToSession(unchecked.session, data, { fetch: unchecked.fetch, checksum: false });
  assert.equal(file.sentChecksum, null);
});

test('a signal stops the transfer here, and abort ends the upload in Cloud', async () => {
  const data = bytes(5000), world = service({ size: 5000 }), controller = new AbortController();
  world.hooks.put = async number => { if (number === 2) controller.abort(); };
  const uploader = createUploader({ authorize: async () => world.session, fetch: world.fetch, concurrency: 1 });
  await assert.rejects(uploader.upload(data, { name: 'a.bin', signal: controller.signal }), error => error.code === 'UPLOAD_CANCELLED' && error.recoverable === true);
  assert.equal((await uploader.status(world.session)).state, 'uploading');
  assert.equal((await uploader.abort(world.session)).state, 'aborted');
  await assert.rejects(uploader.upload(data, { name: 'a.bin' }), error => error.code === 'UPLOAD_SESSION' && error.recoverable === false && /aborted/.test(error.message));
});

test('a refused authorization, another file, a bad session and a failing service are told apart', async () => {
  const data = bytes(1500), world = service({ size: 1500 });
  const refusing = createUploader({ authorize: async () => { throw new Error('403 from backend'); }, fetch: world.fetch });
  await assert.rejects(refusing.upload(data, { name: 'a' }), error => error.code === 'UPLOAD_AUTHORIZATION' && error.cause?.message === '403 from backend');
  await assert.rejects(uploadToSession(world.session, bytes(1499), { fetch: world.fetch }), error => error.code === 'UPLOAD_SOURCE');
  await assert.rejects(uploadToSession({ ...world.session, token: 'rdbo_organization_key' }, data, { fetch: world.fetch }), error => error.code === 'UPLOAD_PROTOCOL');
  await assert.rejects(uploadToSession({ ...world.session, parts: 99 }, data, { fetch: world.fetch }), error => error.code === 'UPLOAD_PROTOCOL');
  await assert.rejects(uploadToSession({ ...world.session, token: `rdbu_${'x'.repeat(43)}` }, data, { fetch: world.fetch }), error => error.code === 'UPLOAD_AUTHORIZATION' && error.status === 401);
  const uploader = createUploader({ authorize: async () => world.session, fetch: world.fetch });
  await assert.rejects(uploader.upload(new Uint8Array(0), { name: 'empty' }), error => error.code === 'UPLOAD_SOURCE');
  await assert.rejects(uploader.upload(data), /name is required/);
  await assert.rejects(uploader.upload('text', { name: 'a' }), /upload takes a File/);
  assert.throws(() => createUploader({}), /authorize function/);
  const down = service({ size: 1500 });
  down.hooks.call = async () => new Response('down', { status: 503 });
  await assert.rejects(uploadToSession(down.session, data, { fetch: down.fetch }), error => error.code === 'UPLOAD_TRANSPORT' && error.recoverable === true && down.requests.length === 4);
});

test('the checksum is the CRC-64/NVME that storage reports', () => {
  const value = crc64nvme().update(new TextEncoder().encode('12345')).update(new TextEncoder().encode('6789')).base64();
  assert.equal(Buffer.from(value, 'base64').toString('hex'), 'ae8b14860a799888');
  assert.equal(crc64nvme().base64(), 'AAAAAAAAAAA=');
});

// ---- db.files, the backend side ----

function backend(reply) {
  const calls = [];
  const fetch = async (url, init) => { calls.push({ url: String(url), method: init.method, headers: init.headers, body: init.body === undefined ? undefined : JSON.parse(init.body), redirect: init.redirect }); return reply(calls.at(-1)); };
  return { calls, fetch };
}

test('db.files sends each call to its route with the connection token', async () => {
  const { calls, fetch } = backend(() => json({ ok: true, files: [], next: null, file: { id: 'f' } }));
  const db = await connect({ url: cloud, token: 'header.payload.signature', fetch, warm: false });
  const file = '01939b2e-7c1a-7000-8000-000000000001', upload = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';
  await db.files.createUploadSession({ name: 'a.png', size: 10, mediaType: 'image/png', subject: 'user-1' });
  await db.files.createUploadSession({ id: upload, name: 'a.png', size: 10 });
  await db.files.uploadStatus(upload);
  assert.deepEqual(await db.files.completeUpload(upload), { id: 'f' });
  await db.files.abortUpload(upload);
  await db.files.get(file);
  await db.files.list();
  await db.files.list({ after: file, limit: 10, order: 'desc' });
  await db.files.update(file, { name: 'b.png' });
  await db.files.createDownloadUrl(file, { expiresInSeconds: 60, disposition: 'inline' });
  await db.files.delete(file);
  await db.files.deletionStatus(file);
  await db.files.usage();
  assert.deepEqual(calls.map(call => `${call.method} ${call.url.slice('https://cloud.romedb.com/v1/organizations/'.length)}`), [
    `POST ${org}/databases/${database}/files/uploads`, `POST ${org}/databases/${database}/files/uploads`,
    `GET ${org}/databases/${database}/files/uploads/${upload}`, `POST ${org}/databases/${database}/files/uploads/${upload}/complete`,
    `POST ${org}/databases/${database}/files/uploads/${upload}/abort`, `GET ${org}/databases/${database}/files/${file}`,
    `GET ${org}/databases/${database}/files`, `GET ${org}/databases/${database}/files?after=${file}&limit=10&order=desc`,
    `PATCH ${org}/databases/${database}/files/${file}`, `POST ${org}/databases/${database}/files/${file}/download`,
    `DELETE ${org}/databases/${database}/files/${file}`, `GET ${org}/databases/${database}/files/${file}/deletion`, `GET ${org}/files/usage`,
  ]);
  assert.ok(calls.every(call => call.headers.authorization === 'Bearer header.payload.signature' && call.redirect === 'manual'));
  assert.match(calls[0].body.id, /^[0-9a-f]{8}-[0-9a-f]{4}-4/);
  assert.deepEqual({ ...calls[0].body, id: 0 }, { id: 0, name: 'a.png', size: 10, mediaType: 'image/png', subject: 'user-1' });
  assert.equal(calls[1].body.id, upload);
  assert.deepEqual(calls[9].body, { expiresInSeconds: 60, disposition: 'inline' });
  assert.equal(db.files, db.files);
  await assert.rejects(db.files.get('not-a-uuid'), /fileId must be a UUID/);
  await assert.rejects(db.files.list({ color: 'red' }), /list takes after, limit and order/);
  await db.close();
  await assert.rejects(db.files.list(), error => error instanceof ClientError && error.code === 'RDB_CLOSED');
});

test('db.files asks for a processing job, reads it, waits for it, cancels it and lists what was made', async () => {
  const file = '01939b2e-7c1a-7000-8000-000000000001', job = 'bbbbbbbb-bbbb-8bbb-8bbb-bbbbbbbbbbbb';
  const states = ['queued', 'running', 'ready'];
  const { calls, fetch } = backend(call => call.method === 'GET' && call.url.endsWith(`/files/jobs/${job}`)
    ? json({ id: job, fileId: file, kind: 'image', recipe: {}, state: states.shift() ?? 'ready', file: { id: 'made' } }) : json({ id: job, state: 'queued', files: [] }));
  const db = await connect({ url: cloud, token: 'header.payload.signature', fetch, warm: false });
  assert.equal((await db.files.process(file, { kind: 'image', recipe: { width: 320, format: 'webp' }, subject: 'user-1' })).id, job);
  assert.equal((await db.files.job(job)).state, 'queued');
  const done = await db.files.waitForJob(job, { intervalMs: 100 });
  assert.deepEqual([done.state, done.file.id], ['ready', 'made']);
  await db.files.cancelJob(job);
  assert.deepEqual(await db.files.derivatives(file), { id: job, state: 'queued', files: [] });
  const base = `${org}/databases/${database}/files`;
  assert.deepEqual(calls.map(call => `${call.method} ${call.url.slice('https://cloud.romedb.com/v1/organizations/'.length)}`), [
    `POST ${base}/${file}/jobs`, `GET ${base}/jobs/${job}`, `GET ${base}/jobs/${job}`, `GET ${base}/jobs/${job}`, `DELETE ${base}/jobs/${job}`, `GET ${base}/${file}/derivatives`,
  ]);
  assert.deepEqual(calls[0].body, { kind: 'image', recipe: { width: 320, format: 'webp' }, subject: 'user-1' });
  await assert.rejects(db.files.process('nope', { kind: 'image', recipe: {} }), /fileId must be a UUID/);
  await assert.rejects(db.files.process(file, 'image'), /a job requires an object/);
  await assert.rejects(db.files.waitForJob(job, { every: 5 }), /waitForJob takes/);
  await assert.rejects(db.files.waitForJob(job, { intervalMs: 5 }), /intervalMs must be/);
  // A job that does not end in time: the wait gives up, and says the job goes on.
  const slow = backend(() => json({ id: job, state: 'running' }));
  const other = await connect({ url: cloud, token: 'header.payload.signature', fetch: slow.fetch, warm: false });
  await assert.rejects(other.files.waitForJob(job, { waitMs: 250, intervalMs: 100 }), error => error instanceof ClientError && error.code === 'RDB_TIMEOUT');
  const controller = new AbortController(); setTimeout(() => controller.abort(), 50);
  await assert.rejects(other.files.waitForJob(job, { signal: controller.signal, intervalMs: 5000 }), error => error.code === 'RDB_CANCELLED');
  await db.close(); await other.close();
});

test('db.files reports refusals as ClientError without the token in the message', async () => {
  const token = 'rdbo_' + 'a'.repeat(64);
  const replies = [json({ error: `Key ${token} lacks the scope` }, 403), json({ error: 'File not found' }, 404), json({ error: 'File storage allowance exceeded' }, 429),
    new Response('bad gateway', { status: 502, headers: { 'retry-after': '2' } }), new Response(null, { status: 302, headers: { location: 'https://elsewhere.example' } })];
  const { fetch } = backend(() => replies.shift());
  const db = await connect({ url: cloud, token, fetch, warm: false });
  const file = '01939b2e-7c1a-7000-8000-000000000001';
  await assert.rejects(db.files.get(file), error => error.code === 'RDB_FORBIDDEN' && error.status === 403 && error.message === 'Key [redacted] lacks the scope');
  await assert.rejects(db.files.get(file), error => error.code === 'RDB_NOT_FOUND' && error.outcome === 'rolled-back');
  await assert.rejects(db.files.createUploadSession({ name: 'a', size: 1 }), error => error.code === 'RDB_LIMIT');
  await assert.rejects(db.files.delete(file), error => error.code === 'RDB_REMOTE' && error.outcome === 'unknown' && error.retryAfterMs === 2000);
  await assert.rejects(db.files.get(file), error => error.code === 'RDB_PROTOCOL' && /redirect/.test(error.message));
  await assert.rejects(db.files.get(file, { signal: AbortSignal.abort() }), error => error.code === 'RDB_CANCELLED' && error.outcome === 'not-sent');
});

test('db.files.upload opens a session and sends the bytes from the backend itself', async () => {
  const data = bytes(2200), world = service({ size: 2200 }), declared = [];
  const fetch = async (url, init) => {
    if (String(url) === `${cloud}/files/uploads`) { declared.push({ authorization: init.headers.authorization, ...JSON.parse(init.body) }); return json(world.session, 201); }
    return world.fetch(url, init);
  };
  const db = await connect({ url: cloud, token: 'header.payload.signature', fetch, warm: false });
  const progress = [];
  const file = await db.files.upload(new Blob([data], { type: 'application/pdf' }), { name: 'report.pdf', metadata: { year: 2026 }, onProgress: step => progress.push(step.uploadedParts) });
  assert.deepEqual(new Uint8Array(file.bytes), data);
  assert.deepEqual({ ...declared[0], id: 0 }, { authorization: 'Bearer header.payload.signature', id: 0, mediaType: 'application/pdf', metadata: { year: 2026 }, name: 'report.pdf', size: 2200 });
  assert.equal(progress.at(-1), 3);
  // After the session is open, only the session token is used.
  assert.ok(world.requests.every(request => request.authorization === `Bearer ${world.session.token}`));
  await assert.rejects(db.files.upload(data), /name is required/);
});
