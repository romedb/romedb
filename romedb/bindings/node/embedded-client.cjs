'use strict';
const { Worker } = require('node:worker_threads');
const { addAbortListener } = require('node:events');
const { randomUUID } = require('node:crypto');
const fs = require('node:fs/promises');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const common = require('./client.cjs');
const { connect, RemoteDatabase, ClientError, Float64, Record, Vector, encodeValue, decodeValue, normalizeUrl, validatePrepared, saveRequest, loadRequest } = common;
const { plainObject } = require('./values.cjs');
const { createEmbeddedClient } = require('./client-internal.cjs');
const native = require('./native.cjs');
const sessions = new Map();
const construction = Symbol('EmbeddedDatabase');

async function openEmbedded(options) {
  const duration = options?.timeoutMs ?? 20000;
  if (!Number.isInteger(duration) || duration < 1 || duration > 120000) throw new RangeError('timeoutMs must be 1..120000');
  let abandoned = false;
  const opening = openEmbeddedFile(options);
  opening.then(database => { if (abandoned) void database.close().catch(() => {}); }, () => {});
  let timer;
  const deadline = new Promise((resolve, reject) => {
    timer = setTimeout(() => { abandoned = true; reject(new common.ClientError('FDB_CANCELLED', 'Embedded connection deadline expired; startup cleanup will finish when native work returns', 'not-sent')); }, duration);
  });
  try { return await Promise.race([opening, deadline]); }
  catch (error) {
    if (error instanceof common.ClientError || error instanceof TypeError || error instanceof RangeError) throw error;
    const code = error.code === 'EEXIST' ? 'FDB_ALREADY_EXISTS' : ['EACCES', 'EPERM'].includes(error.code) ? 'FDB_FORBIDDEN' : error.code === 'ENOENT' ? 'FDB_NOT_FOUND' : 'FDB_STORAGE';
    throw new common.ClientError(code, 'Embedded filesystem operation failed', 'not-sent');
  } finally { clearTimeout(timer); }
}

async function openEmbeddedFile(options) {
  if (!plainObject(options) || Object.keys(options).some(key => !['path', 'create', 'timeoutMs'].includes(key)) || typeof options.path !== 'string' || !options.path || options.path.length > 4096 || options.path.includes('\0') || (options.create !== undefined && typeof options.create !== 'boolean')) throw new TypeError('embedded options require a bounded path, optional create and timeoutMs');
  if (options.timeoutMs !== undefined && (!Number.isInteger(options.timeoutMs) || options.timeoutMs < 1 || options.timeoutMs > 120000)) throw new RangeError('timeoutMs must be 1..120000');
  const file = path.resolve(options.path);
  if (options.create) {
    const parsed = path.parse(file);
    for (const sidecar of [file + '-wal', file + '-shm', file + '-log', file + '-journal', path.join(parsed.dir, parsed.name + '.db-log')]) {
      try { await fs.lstat(sidecar); throw new common.ClientError('FDB_CONFLICT', 'Database sidecar exists; recover it before creating a file', 'not-sent'); }
      catch (error) { if (error.code !== 'ENOENT') throw error; }
    }
    const handle = await fs.open(file, 'wx', 0o600);
    try { await handle.sync(); } finally { await handle.close(); }
    const parent = await fs.open(path.dirname(file), 'r');
    try { await parent.sync(); } finally { await parent.close(); }
  }
  let metadata;
  try { metadata = await fs.lstat(file); }
  catch (error) {
    if (error.code === 'ENOENT') throw new common.ClientError('FDB_NOT_FOUND', 'Open an existing database or explicitly create one', 'not-sent');
    throw error;
  }
  if (!metadata.isFile() || metadata.nlink !== 1) throw new common.ClientError('FDB_VALIDATION', 'Use a regular database file with one link', 'not-sent');
  const canonical = await fs.realpath(file);
  let session = sessions.get(canonical);
  if (session?.closing) { await session.closed; return openEmbeddedFile({ ...options, create: false }); }
  if (!session) {
    if (sessions.size >= 16) throw new common.ClientError('FDB_LIMIT', 'Embedded database connection limit reached', 'not-sent');
    session = new Session(canonical);
    sessions.set(canonical, session);
  }
  session.references++;
  try {
    await session.ready;
    const client = await createEmbeddedClient({ url: session.url, timeoutMs: options.timeoutMs }, (route, body, signal, duration) => session.request(route === '' ? 'info' : route === '/read' ? 'read' : 'write', body, signal, duration));
    return new EmbeddedDatabase(client, session, canonical, construction);
  } catch (error) { await releaseSession(session, canonical); throw error; }
}

class EmbeddedDatabase {
  #client; #session; #path; #closing;
  constructor(client, session, file, key) {
    if (key !== construction) throw new TypeError('use openEmbedded()');
    this.#client = client; this.#session = session; this.#path = file;
  }
  get url() { return this.#client.url; }
  get sequence() { return this.#client.sequence; }
  get databaseId() { return this.#client.databaseId; }
  get path() { return this.#path; }
  info(options) { return this.#client.info(options); }
  read(statements, options) { return this.#client.read(statements, options); }
  prepareWrite(statements, options) { return this.#client.prepareWrite(statements, options); }
  submit(request, options) { return this.#client.submit(request, options); }
  recover(request, options) { return this.#client.recover(request, options); }
  writeAtomic(statements, options) { return this.#client.writeAtomic(statements, options); }
  close() {
    this.#closing ??= (async () => { await this.#client.close(); await releaseSession(this.#session, this.#path); })();
    return this.#closing;
  }
}

class Session {
  #worker; #pending = new Map(); #bytes = 0; #next = 0; #failure;
  references = 0; closing = false; ready; closed;
  constructor(file) {
    this.url = pathToFileURL(file).href;
    this.#worker = new Worker(require.resolve('./embedded-worker.cjs'), { workerData: { path: file, url: this.url, databaseId: randomUUID() } });
    let resolveReady, rejectReady, resolveExit;
    this.ready = new Promise((resolve, reject) => { resolveReady = resolve; rejectReady = reject; });
    this.closed = new Promise(resolve => { resolveExit = resolve; });
    this.ready.catch(() => {});
    this.#worker.on('message', message => {
      if (message.ready) return resolveReady();
      if (message.readyError) {
        this.#failure = new common.ClientError(message.readyError.code, message.readyError.message, 'not-sent');
        rejectReady(this.#failure);
        return;
      }
      const pending = this.#pending.get(message.id);
      if (!pending) return;
      this.#pending.delete(message.id);
      this.#bytes -= pending.bytes;
      pending.listener?.[Symbol.dispose]();
      native.releaseCancellationToken(pending.token);
      if (message.error) pending.reject(new common.ClientError(message.error.code, message.error.message, message.error.outcome));
      else pending.resolve(message.result);
    });
    this.#worker.on('error', () => {
      this.#failure ??= new common.ClientError('FDB_TRANSPORT', 'Embedded worker failed; reconcile pending writes', 'unknown');
      rejectReady(this.#failure);
    });
    this.#worker.on('exit', code => {
      this.#failure ??= new common.ClientError('FDB_CLOSED', code === 0 ? 'Embedded worker closed' : 'Embedded worker exited unexpectedly', 'unknown');
      rejectReady(this.#failure);
      for (const pending of this.#pending.values()) {
        pending.listener?.[Symbol.dispose]();
        native.releaseCancellationToken(pending.token);
        pending.reject(this.#failure);
      }
      this.#pending.clear(); this.#bytes = 0;
      resolveExit();
    });
  }
  request(operation, body, signal, duration) {
    if (this.#failure) return Promise.reject(this.#failure);
    if (this.closing && operation !== 'close') return Promise.reject(new common.ClientError('FDB_CLOSED', 'Embedded session is closing', 'not-sent'));
    if (signal?.aborted) return Promise.reject(new common.ClientError('FDB_CANCELLED', 'Request cancelled before dispatch', 'not-sent'));
    const bytes = body === undefined ? 0 : Buffer.byteLength(body);
    if (operation !== 'close' && (this.#pending.size >= 32 || this.#bytes + bytes > 2097152)) return Promise.reject(new common.ClientError('FDB_LIMIT', 'Embedded request queue limit reached', 'not-sent'));
    const token = native.createCancellationToken(duration === undefined ? undefined : String(duration));
    return new Promise((resolve, reject) => {
      const id = ++this.#next;
      const listener = signal === undefined ? undefined : addAbortListener(signal, () => {
        native.cancelOperation(token);
        reject(new common.ClientError('FDB_CANCELLED', operation === 'write' ? 'Write was not confirmed; recover the saved request' : 'Embedded request cancelled', operation === 'write' ? 'unknown' : 'read-failed'));
      });
      this.#pending.set(id, { resolve, reject, token, listener, bytes }); this.#bytes += bytes;
      try { this.#worker.postMessage({ id, operation, body, cancellationKey: token }); }
      catch {
        this.#pending.delete(id); this.#bytes -= bytes;
        listener?.[Symbol.dispose](); native.releaseCancellationToken(token);
        reject(new common.ClientError('FDB_TRANSPORT', 'Embedded request could not be dispatched', 'not-sent'));
      }
    });
  }
  async close() {
    this.closing = true;
    try { if (!this.#failure) await this.request('close'); }
    finally { await this.closed; }
  }
}

async function releaseSession(session, file) {
  if (--session.references === 0) {
    try { await session.close(); }
    finally { if (sessions.get(file) === session) sessions.delete(file); }
  }
}

module.exports = { openEmbedded, EmbeddedDatabase, connect, RemoteDatabase, ClientError, Float64, Record, Vector, encodeValue, decodeValue, normalizeUrl, validatePrepared, saveRequest, loadRequest };
