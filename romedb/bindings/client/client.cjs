'use strict';
// Web APIs only: this file also runs in Workers and browsers through client.mjs.
const { Float64, Record, Vector, encodeValue, decodeValue, plainObject, text, checkCopyOptions } = require('./values.cjs');

const CLOUD_QUERY_VERSION = 3;
const MAX_RESPONSE = 524_288;
const EARLIER_LIMITS = Object.freeze({ maxRequestBytes: 65_536, maxStatements: 32, maxRows: 1000, maxResultBytes: 262_144, maxResponseBytes: MAX_RESPONSE });
const CLOUD_LIMITS = Object.freeze({ maxRequestBytes: 1_048_576, maxStatements: 256, maxRows: 10_000, maxResultBytes: 8_388_608, maxResponseBytes: 9_437_184, maxCommitBytes: 1_048_576, maxCommitRows: 100_000 });
const REPORTED_LIMIT_CEILINGS = Object.freeze({ maxRequestBytes: 64 * 1024 * 1024, maxStatements: 4096, maxRows: 1_000_000, maxResultBytes: 64 * 1024 * 1024, maxResponseBytes: 128 * 1024 * 1024, maxCommitBytes: 4 * 1024 * 1024 * 1024, maxCommitRows: 1_000_000_000 });
const COMMIT_LIMITS = Object.freeze(['maxCommitBytes', 'maxCommitRows']);
const MAX_TIMEOUT_MS = 600_000;
const MAX_SEQUENCE = Number.MAX_SAFE_INTEGER - 1;
const DATABASE_FILE_TYPE = 'application/vnd.romedb.database';
const construction = Symbol('RemoteDatabase');
const warmAtConnect = Symbol('warmAtConnect');
const WARM_STATES = new Set(['ready', 'starting', 'skipped']);
const encoder = new TextEncoder();

class ClientError extends Error {
  constructor(code, message, outcome, details = {}) {
    const { cause, ...fields } = details;
    super(message, cause === undefined ? undefined : { cause });
    this.name = 'ClientError';
    this.code = code;
    this.outcome = outcome;
    Object.assign(this, fields);
  }
}

async function connect(options) {
  const db = new RemoteDatabase(options, construction);
  db[warmAtConnect]();
  if (db.deferred) return db;
  try { await db.info(); return db; }
  catch (error) { await db.close(); throw error; }
}

class RemoteDatabase {
  #url; #token; #fetch; #timeout; #sequence = 0; #id; #capabilities; #limits = EARLIER_LIMITS; #transport;
  #closed = false; #active = new Set(); #inflightBytes = 0; #sequenceKnown = false; #deferred = false; #cloud = false; #warm = false; #loopbackOrganization = false;
  constructor(options, key, transport) {
    if (key !== construction) throw new TypeError('use connect()');
    if (!plainObject(options)) throw new TypeError('connection options require an object');
    this.#url = transport ? normalizeFileUrl(options.url) : normalizeUrl(options.url);
    this.#transport = transport;
    if (typeof options.token !== 'string' || !/^[\x21-\x7e]{1,4096}$/.test(options.token)) throw new TypeError('token requires a bounded bearer credential');
    this.#token = options.token;
    const fetch = options.fetch ?? globalThis.fetch;
    if (typeof fetch !== 'function') throw new TypeError('fetch must be a function');
    // Workers refuse fetch called on another object, so the default keeps globalThis.
    this.#fetch = options.fetch ?? fetch.bind(globalThis);
    this.#timeout = timeout(options.timeoutMs ?? 20_000);
    if (options.deferInfo !== undefined && typeof options.deferInfo !== 'boolean') throw new TypeError('deferInfo requires a boolean');
    if (options.warm !== undefined && typeof options.warm !== 'boolean') throw new TypeError('warm requires a boolean');
    const cloudId = transport ? undefined : /\/databases\/([0-9a-f-]{36})$/.exec(this.#url)?.[1];
    // A loopback organization URL is a local service directory or Cloud in development;
    // the details reply tells which, so these connections always read details first.
    this.#loopbackOrganization = cloudId !== undefined && loopbackHost(new URL(this.#url).hostname);
    this.#cloud = cloudId !== undefined && !this.#loopbackOrganization;
    if (cloudId === undefined && options.deferInfo === true) throw new TypeError('deferInfo applies only to Cloud database URLs');
    if (cloudId === undefined && options.warm === true) throw new TypeError('warm applies only to Cloud database URLs');
    // Cloud database URLs carry the database id and have documented capabilities,
    // so the details request waits until a write needs a current sequence.
    if (this.#cloud && options.deferInfo !== false) {
      this.#id = cloudId; this.#capabilities = capabilities(this.#url, {}, false); this.#limits = CLOUD_LIMITS; this.#deferred = true;
    }
    this.#warm = this.#cloud && options.warm !== false;
  }
  [warmAtConnect]() {
    if (this.#warm) this.warm().catch(() => {});
  }
  get deferred() { return this.#deferred; }
  get url() { return this.#url; }
  get cloud() { return this.#cloud; }
  get sequence() { return this.#sequence; }
  get sequenceKnown() { return this.#sequenceKnown; }
  get databaseId() { return this.#id; }
  get limits() { return this.#limits; }
  async info(options = {}) {
    const info = await this.#request('', undefined, options, false);
    if (!plainObject(info) || typeof info.id !== 'string' || !/^[A-Za-z0-9_-]{1,80}$/.test(info.id) || !sequence(info.sequence, true) || (this.#id !== undefined && this.#id !== info.id)) {
      throw new ClientError('RDB_PROTOCOL', 'Invalid database details or changed database identity', 'read-failed');
    }
    if (this.#loopbackOrganization) this.#cloud = info.serviceVersion === undefined;
    const supported = capabilities(this.#url, info, !this.#cloud);
    const limits = requestLimits(info.limits, this.#cloud ? CLOUD_LIMITS : EARLIER_LIMITS);
    this.#id = info.id;
    this.#limits = limits;
    this.#sequence = Math.max(this.#sequence, info.sequence);
    this.#sequenceKnown = true;
    this.#capabilities = supported;
    return { ...info, capabilities: supported };
  }
  async warm(options = {}) {
    this.#assertOpen();
    if (!this.#cloud) throw new ClientError('RDB_UNSUPPORTED', 'warm applies only to Cloud database URLs', 'not-sent');
    const reply = await this.#request('/warm', '{}', options, false);
    if (!plainObject(reply) || !WARM_STATES.has(reply.state)) throw new ClientError('RDB_PROTOCOL', 'Invalid warm reply', 'read-failed');
    return { state: reply.state };
  }
  async read(statements, options = {}) {
    this.#assertOpen();
    if (this.#capabilities.readVersion !== 2) throw new ClientError('RDB_UNSUPPORTED', 'Database does not support fresh reads', 'not-sent');
    if (options.ifSequence !== undefined) throw new TypeError('fresh reads do not accept ifSequence');
    const afterSequence = options.afterSequence ?? this.#sequence;
    checkSequence(afterSequence);
    const workLimits = encodeWorkLimits(options.workLimits);
    const queryVersion = this.#queryVersion();
    const body = JSON.stringify({ readVersion: 2, requestId: crypto.randomUUID(), afterSequence, statements: encodeStatements(statements, queryVersion, this.#limits.maxStatements), ...(workLimits === undefined ? {} : { workLimits }) });
    checkBodySize(body, this.#limits.maxRequestBytes);
    const request = JSON.parse(body);
    const reply = await this.#request('/read', body, options, false, undefined, queryVersion);
    const result = confirmReply(reply, request, true, undefined, this.#limits);
    this.#sequence = Math.max(this.#sequence, result.sequence);
    this.#sequenceKnown = true;
    return result;
  }
  prepareWrite(statements, options = {}) {
    this.#assertOpen();
    if (!this.#sequenceKnown && this.#deferred && options.afterSequence === undefined) throw new ClientError('RDB_SEQUENCE_UNKNOWN', 'Read, call info() or pass afterSequence before preparing a write on a deferred connection', 'not-sent');
    if (this.#capabilities.atomicWrites !== true || this.#capabilities.replayWindow !== 64) throw new ClientError('RDB_UNSUPPORTED', 'Database does not support atomic writes with recovery', 'not-sent');
    const afterSequence = options.afterSequence ?? this.#sequence;
    checkSequence(afterSequence);
    if (options.ifSequence !== undefined) {
      if (this.#capabilities.exactSequenceCondition !== true) throw new ClientError('RDB_UNSUPPORTED', 'Database does not support exact sequence conditions', 'not-sent');
      checkSequence(options.ifSequence);
      if (options.ifSequence < afterSequence) throw new RangeError('ifSequence must be at least afterSequence');
    }
    const requestId = options.requestId ?? crypto.randomUUID();
    if (typeof requestId !== 'string' || !/^[A-Za-z0-9_-]{1,80}$/.test(requestId)) throw new TypeError('requestId requires 1..80 letters, digits, underscores or hyphens');
    const workLimits = encodeWorkLimits(options.workLimits);
    const queryVersion = this.#queryVersion();
    const body = JSON.stringify({ requestId, afterSequence, ...(options.ifSequence === undefined ? {} : { ifSequence: options.ifSequence }), statements: encodeStatements(statements, queryVersion, this.#limits.maxStatements), ...(workLimits === undefined ? {} : { workLimits }) });
    checkBodySize(body, this.#limits.maxRequestBytes);
    return Object.freeze({ version: 2, url: this.#url, databaseId: this.#id, queryVersion, body });
  }
  async submit(prepared, options = {}) {
    this.#assertOpen();
    const request = validatePrepared(prepared);
    if (this.#capabilities.atomicWrites !== true || this.#capabilities.replayWindow !== 64 || (request.ifSequence !== undefined && this.#capabilities.exactSequenceCondition !== true)) throw new ClientError('RDB_UNSUPPORTED', 'Database does not support this write contract', 'not-sent', { request: prepared });
    if (prepared.url !== this.#url || prepared.databaseId !== this.#id) throw new ClientError('RDB_REQUEST_IDENTITY', 'Saved request belongs to another database', 'not-sent');
    if (byteLength(prepared.body) > this.#limits.maxRequestBytes || request.statements.length > this.#limits.maxStatements) throw new ClientError('RDB_LIMIT', 'Saved request exceeds the request limits of this database', 'not-sent', { request: prepared });
    try {
      const reply = await this.#request('/query', prepared.body, options, true, prepared, preparedQueryVersion(prepared));
      const result = confirmReply(reply, request, false, prepared, this.#limits);
      this.#sequence = Math.max(this.#sequence, result.sequence);
      this.#sequenceKnown = true;
      return result;
    } catch (error) {
      if (error instanceof ClientError) {
        if (error.outcome === 'rolled-back') this.#sequence = Math.max(this.#sequence, error.sequence);
        throw error;
      }
      throw new ClientError('RDB_PROTOCOL', 'Write response could not be confirmed; recover the saved request', 'unknown', { request: prepared });
    }
  }
  async recover(prepared, options = {}) {
    try { return await this.submit(prepared, options); }
    catch (error) {
      if (error instanceof ClientError && error.outcome === 'not-sent') throw new ClientError(error.code, 'Recovery did not confirm the earlier write; keep the saved request', 'unknown', { request: prepared });
      throw error;
    }
  }
  async writeAtomic(statements, options = {}) {
    if (!this.#sequenceKnown && this.#deferred && options.afterSequence === undefined) await this.info(options);
    return this.submit(this.prepareWrite(statements, options), options);
  }
  async exportFile(options = {}) {
    this.#assertOpen();
    if (this.#transport) throw new ClientError('RDB_UNSUPPORTED', 'Export embedded databases through EmbeddedDatabase.exportFile', 'not-sent');
    if (!plainObject(options) || Object.keys(options).some(key => !['signal', 'timeoutMs', 'waitMs', 'emptyTables', 'newIdentity'].includes(key))) throw new TypeError('export options take signal, timeoutMs, emptyTables and newIdentity');
    checkCopyOptions(options);
    const duration = timeout(options.timeoutMs ?? this.#timeout);
    this.#checkJob(options);
    const finished = this.#cloud ? await this.#exportJob(options, duration) : undefined;
    const controller = new AbortController();
    const abort = () => controller.abort();
    options.signal?.addEventListener('abort', abort, { once: true });
    const timer = setTimeout(abort, duration);
    this.#active.add(controller);
    let open = true;
    const finish = () => {
      if (!open) return;
      open = false;
      clearTimeout(timer);
      options.signal?.removeEventListener('abort', abort);
      this.#active.delete(controller);
    };
    const credential = this.#token;
    let status;
    try {
      const response = await this.#fetch(this.#url + (finished ?? '/file' + fileQuery(options)), {
        method: 'GET',
        headers: { authorization: `Bearer ${credential}`, accept: `${DATABASE_FILE_TYPE}, application/json`, ...(this.#id === undefined ? {} : { 'x-romedb-database-id': this.#id }) },
        redirect: 'manual', signal: controller.signal,
      });
      // The deadline covers building the copy; the caller's signal still stops the download.
      clearTimeout(timer);
      status = response.status;
      if (response.type === 'opaqueredirect' || (status >= 300 && status < 400)) throw new ClientError('RDB_PROTOCOL', 'Database URL redirected; redirects are never followed with a credential', 'read-failed', { status });
      if (!response.ok) {
        const retryAfterMs = retryAfter(response.headers.get('retry-after'));
        const reply = await boundedJson(response, MAX_RESPONSE).catch(() => undefined);
        const code = plainObject(reply?.error) && typeof reply.error.code === 'string' && /^RDB_[A-Z0-9_]{1,80}$/.test(reply.error.code) ? reply.error.code : httpCode(status);
        throw new ClientError(code, safeMessage(reply, credential), 'read-failed', { status, ...(retryAfterMs === undefined ? {} : { retryAfterMs }) });
      }
      const size = Number(/^\d{1,16}$/.exec(response.headers.get('content-length') ?? '')?.[0] ?? NaN);
      const exported = Number(/^\d{1,16}$/.exec(response.headers.get('x-romedb-sequence') ?? '')?.[0] ?? NaN);
      const identity = response.headers.get('x-romedb-database-id');
      if ((response.headers.get('content-type') ?? '').split(';')[0].trim().toLowerCase() !== DATABASE_FILE_TYPE || !Number.isSafeInteger(size) || !sequence(exported, true) || !response.body) {
        throw new ClientError('RDB_PROTOCOL', 'Invalid database file response', 'read-failed', { status });
      }
      if (identity === null || (this.#id !== undefined && identity !== this.#id)) throw new ClientError('RDB_REQUEST_IDENTITY', 'Database file belongs to another database', 'read-failed', { status });
      this.#id ??= identity;
      this.#sequence = Math.max(this.#sequence, exported);
      this.#sequenceKnown = true;
      return { sequence: exported, size, body: countedBody(response.body, size, controller, finish) };
    } catch (error) {
      finish();
      if (error instanceof ClientError) throw new ClientError(error.code, redactMessage(error.message, credential), error.outcome, { ...(error.status === undefined ? {} : { status: error.status }), ...(error.retryAfterMs === undefined ? {} : { retryAfterMs: error.retryAfterMs }) });
      throw new ClientError(controller.signal.aborted ? 'RDB_CANCELLED' : status === undefined ? 'RDB_TRANSPORT' : 'RDB_PROTOCOL', 'Database file request failed', 'read-failed', { ...(status === undefined ? {} : { status }), cause: error });
    }
  }
  // Cloud builds a file in a job that outlasts any one request, so this starts the job
  // and waits for it. It gives the path of the finished file, or undefined from a
  // service without jobs, whose `/file` builds the copy while the request waits.
  async #exportJob(options, duration) {
    const job = await this.#job('/file-exports', 'file export', options, duration, { query: fileQuery(options), optional: true });
    return job && `/file-exports/${job.id}/file`;
  }
  // Starts a job and waits for it, up to waitMs, and gives its finished state. A job
  // with `cancel` is canceled when the wait ends early, because nobody is left to
  // answer for it. With `optional`, a service without such jobs gives undefined.
  async #job(route, name, options, duration, { query = '', body, optional = false, cancel = false, failure = 'RDB_STORAGE', outcome = 'read-failed' }) {
    const controller = new AbortController();
    const abort = () => controller.abort();
    options.signal?.addEventListener('abort', abort, { once: true });
    this.#active.add(controller);
    const credential = this.#token, deadline = Date.now() + (options.waitMs ?? MAX_EXPORT_WAIT);
    const label = name[0].toUpperCase() + name.slice(1);
    const headers = { authorization: `Bearer ${credential}`, accept: 'application/json', ...(this.#id === undefined ? {} : { 'x-romedb-database-id': this.#id }) };
    const call = (method, path, content, signal = AbortSignal.any([controller.signal, AbortSignal.timeout(duration)])) => this.#fetch(this.#url + path, {
      method, redirect: 'manual', signal,
      headers: content === undefined ? headers : { ...headers, 'content-type': 'application/json' },
      ...(content === undefined ? {} : { body: content }),
    });
    const refused = async response => {
      const retryAfterMs = retryAfter(response.headers.get('retry-after'));
      const reply = await boundedJson(response, MAX_RESPONSE).catch(() => undefined);
      const code = plainObject(reply?.error) && typeof reply.error.code === 'string' && /^RDB_[A-Z0-9_]{1,80}$/.test(reply.error.code) ? reply.error.code : httpCode(response.status);
      return new ClientError(code, safeMessage(reply, credential), 'read-failed', { status: response.status, ...(retryAfterMs === undefined ? {} : { retryAfterMs }) });
    };
    let running;
    try {
      const started = await call('POST', route + query, body);
      if (optional && [404, 405, 503].includes(started.status) && started.headers.get('retry-after') === null) { await started.body?.cancel(); return undefined; }
      if (started.status !== 202) throw await refused(started);
      const job = await boundedJson(started, MAX_RESPONSE);
      if (!plainObject(job) || typeof job.id !== 'string' || !/^[0-9a-f-]{36}$/.test(job.id)) throw new ClientError('RDB_PROTOCOL', `Invalid ${name} response`, 'read-failed');
      running = job.id;
      for (let failures = 0; ;) {
        if (Date.now() >= deadline) throw new ClientError('RDB_TIMEOUT', `The ${name} is still running`, 'read-failed');
        await pause(EXPORT_POLL_MS, controller.signal);
        const reply = await call('GET', `${route}/${job.id}`).catch(error => {
          if (controller.signal.aborted || ++failures > EXPORT_POLL_FAILURES) throw error;
        });
        if (!reply) continue;
        if (!reply.ok) throw await refused(reply);
        failures = 0;
        const state = await boundedJson(reply, MAX_RESPONSE);
        if (state?.state === 'ready') { running = undefined; return state; }
        if (state?.state === 'failed') { running = undefined; throw new ClientError(failure, typeof state.error === 'string' ? state.error : `The ${name} did not finish`, outcome); }
        if (state?.state !== 'running') throw new ClientError('RDB_PROTOCOL', `Invalid ${name} state`, 'read-failed');
      }
    } catch (error) {
      if (cancel && running !== undefined) await call('DELETE', `${route}/${running}`, undefined, AbortSignal.timeout(Math.min(duration, CANCEL_MS))).then(reply => reply.body?.cancel()).catch(() => {});
      if (error instanceof ClientError) throw error;
      throw new ClientError(controller.signal.aborted ? 'RDB_CANCELLED' : 'RDB_TRANSPORT', `${label} request failed`, 'read-failed');
    } finally {
      options.signal?.removeEventListener('abort', abort);
      this.#active.delete(controller);
    }
  }
  async buildIndex(sql, options = {}) {
    this.#assertOpen();
    if (!this.#cloud) throw new ClientError('RDB_UNSUPPORTED', 'buildIndex applies only to Cloud database URLs; run CREATE INDEX as a query', 'not-sent');
    if (typeof sql !== 'string' || !sql.trim()) throw new TypeError('sql must be one CREATE INDEX statement');
    if (!plainObject(options) || Object.keys(options).some(key => !['signal', 'timeoutMs', 'waitMs'].includes(key))) throw new TypeError('index build options take signal, timeoutMs and waitMs');
    const duration = timeout(options.timeoutMs ?? this.#timeout);
    this.#checkJob(options);
    const job = await this.#job('/index-builds', 'index build', options, duration, { body: JSON.stringify({ sql }), cancel: true, failure: 'RDB_INDEX_BUILD', outcome: 'rolled-back' });
    return { id: job.id, bytes: job.bytes };
  }
  #checkJob(options) {
    if (options.signal !== undefined && !(options.signal instanceof AbortSignal)) throw new TypeError('signal must be an AbortSignal');
    if (options.signal?.aborted) throw new ClientError('RDB_CANCELLED', 'Request cancelled before dispatch', 'not-sent');
    if (this.#active.size >= 32) throw new ClientError('RDB_LIMIT', 'Client request limit reached', 'not-sent');
    if (options.waitMs !== undefined && (!Number.isInteger(options.waitMs) || options.waitMs < 1 || options.waitMs > MAX_EXPORT_WAIT)) throw new RangeError(`waitMs must be 1..${MAX_EXPORT_WAIT}`);
  }
  async close() {
    this.#closed = true;
    for (const controller of this.#active) controller.abort();
    this.#token = '';
  }
  #assertOpen() {
    if (this.#closed) throw new ClientError('RDB_CLOSED', 'Database client is closed', 'not-sent');
  }
  #queryVersion() { return this.#cloud ? CLOUD_QUERY_VERSION : 2; }
  async #request(path, body, options, writing, prepared, queryVersion = 2) {
    this.#assertOpen();
    const duration = timeout(options.timeoutMs ?? this.#timeout);
    if (options.signal !== undefined && !(options.signal instanceof AbortSignal)) throw new TypeError('signal must be an AbortSignal');
    if (options.signal?.aborted) throw new ClientError('RDB_CANCELLED', 'Request cancelled before dispatch', 'not-sent', prepared ? { request: prepared } : {});
    const bodyBytes = body === undefined ? 0 : byteLength(body);
    if (this.#active.size >= 32 || this.#inflightBytes + bodyBytes > Math.max(2_097_152, 2 * this.#limits.maxRequestBytes)) throw new ClientError('RDB_LIMIT', 'Client request limit reached', 'not-sent', prepared ? { request: prepared } : {});
    const controller = new AbortController();
    const abort = () => controller.abort();
    options.signal?.addEventListener('abort', abort, { once: true });
    const timer = setTimeout(abort, duration);
    this.#active.add(controller);
    this.#inflightBytes += bodyBytes;
    const credential = this.#token;
    const fetch = this.#fetch;
    let status;
    try {
      if (this.#transport) return await this.#transport(path, body, controller.signal, duration);
      const send = () => fetch(this.#url + path, {
        method: body === undefined ? 'GET' : 'POST',
        headers: { authorization: `Bearer ${credential}`, accept: 'application/json', ...(body === undefined ? {} : { 'content-type': 'application/json' }), ...(this.#id === undefined ? {} : { 'x-romedb-database-id': prepared?.databaseId ?? this.#id }),
          ...(queryVersion === CLOUD_QUERY_VERSION && (path === '/query' || path === '/read') ? { 'x-romedb-query-version': String(queryVersion) } : {}),
          ...(path === '/query' || path === '/read' ? { 'x-romedb-result-limits': `rows=${this.#limits.maxRows}; bytes=${this.#limits.maxResultBytes}` } : {}) },
        ...(body === undefined ? {} : { body }),
        // Workers have no redirect: 'error'; a redirect is refused below, never followed with the credential.
        redirect: 'manual', signal: controller.signal,
      });
      let response = await send();
      if (await queueRefused(response)) {
        await pause(queueRetryDelay(response), controller.signal);
        response = await send();
      }
      status = response.status;
      if (response.type === 'opaqueredirect' || (status >= 300 && status < 400)) {
        await response.body?.cancel().catch(() => {});
        throw new ClientError('RDB_PROTOCOL', 'Database URL redirected; redirects are never followed with a credential', writing ? 'unknown' : 'read-failed', { status, ...(prepared ? { request: prepared } : {}) });
      }
      const retryAfterMs = retryAfter(response.headers.get('retry-after'));
      const replyVersion = response.headers.get('x-romedb-query-version');
      if ((path === '/query' || path === '/read') && replyVersion !== null && replyVersion !== '2' && replyVersion !== String(queryVersion)) {
        await response.body?.cancel();
        throw new ClientError('RDB_PROTOCOL', 'Unsupported database query response version', writing ? 'unknown' : 'read-failed', { status, ...(prepared ? { request: prepared } : {}) });
      }
      const reply = await boundedJson(response, this.#limits.maxResponseBytes);
      if (plainObject(reply?.error) && typeof reply.error.message === 'string') reply.error.message = redactMessage(reply.error.message, credential);
      if (!response.ok && !(writing && reply?.requestId && reply?.sequence !== undefined)) {
        const code = plainObject(reply?.error) && typeof reply.error.code === 'string' && /^RDB_[A-Z0-9_]{1,80}$/.test(reply.error.code) ? reply.error.code : httpCode(status);
        throw new ClientError(code, safeMessage(reply, credential), writing ? 'unknown' : 'read-failed', { status, ...(retryAfterMs === undefined ? {} : { retryAfterMs }), ...(prepared ? { request: prepared } : {}) });
      }
      return reply;
    } catch (error) {
      if (error instanceof ClientError) {
        const outcome = writing
          ? this.#transport && error.outcome === 'not-sent' ? 'not-sent' : 'unknown'
          : error.outcome === 'not-sent' ? 'not-sent' : 'read-failed';
        const message = this.#transport ? error.message.slice(0, 2048) : redactMessage(error.message, credential);
        throw new ClientError(error.code, message, outcome, {
          ...(error.status === undefined ? {} : { status: error.status }),
          ...(error.retryAfterMs === undefined ? {} : { retryAfterMs: error.retryAfterMs }),
          ...(prepared ? { request: prepared } : {}),
        });
      }
      const code = controller.signal.aborted ? 'RDB_CANCELLED' : status === undefined ? 'RDB_TRANSPORT' : 'RDB_PROTOCOL';
      throw new ClientError(code, writing ? 'Write was not confirmed; recover the saved request' : 'Database request failed', writing ? 'unknown' : 'read-failed', { ...(status === undefined ? {} : { status }), ...(prepared ? { request: prepared } : {}), cause: error });
    } finally {
      clearTimeout(timer);
      options.signal?.removeEventListener('abort', abort);
      this.#active.delete(controller);
      this.#inflightBytes -= bodyBytes;
    }
  }
}

require('./client-internal.cjs').createEmbeddedClient = async (options, transport) => {
  const database = new RemoteDatabase({ ...options, token: 'embedded' }, construction, transport);
  try { await database.info(); return database; }
  catch (error) { await database.close(); throw error; }
};

function normalizeFileUrl(input) {
  if (typeof input !== 'string' || input.length > 4096) throw new TypeError('file URL requires a bounded local path');
  const url = new URL(input);
  if (url.protocol !== 'file:' || url.hostname || url.search || url.hash || url.pathname === '/' || /%2f|%5c|%00/i.test(url.pathname)) throw new TypeError('use a local file URL without a query or fragment');
  return url.href;
}

function normalizeUrl(input) {
  if (typeof input !== 'string' || input.length > 4096) throw new TypeError('url requires a bounded database URL');
  // romedb:// is an alias for https://; the same checks apply after the rewrite.
  const url = new URL(input.startsWith('romedb://') ? 'https://' + input.slice('romedb://'.length) : input);
  const loopback = loopbackHost(url.hostname);
  if (url.username || url.password || url.search || url.hash || (url.protocol !== 'https:' && !(url.protocol === 'http:' && loopback))) throw new TypeError('use HTTPS or loopback HTTP without URL credentials, query or fragment');
  let path = url.pathname.replace(/\/$/, '').replace(/\/(?:query|read)$/, '');
  if (loopback && !path) path = '/v1/database';
  const uuid = '[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}';
  if (!(loopback && path === '/v1/database') && !new RegExp(`^/v1/organizations/${uuid}/databases/${uuid}$`).test(path)) throw new TypeError('use a local service URL or stable organization-scoped Cloud database URL');
  return url.origin + path;
}

function encodeStatements(statements, queryVersion, maxStatements) {
  if (!Array.isArray(statements) || statements.length < 1 || statements.length > maxStatements) throw new RangeError(`request requires 1..${maxStatements} statements`);
  return statements.map(statement => {
    if (!plainObject(statement) || Object.keys(statement).some(key => !['sql', 'params'].includes(key)) || typeof statement.sql !== 'string' || !statement.sql.trim()) throw new TypeError('statement requires SQL and optional params');
    if (statement.params === undefined) return { sql: text(statement.sql) };
    if (!plainObject(statement.params)) throw new TypeError('params require an object');
    return { sql: text(statement.sql), params: Object.fromEntries(Object.entries(statement.params).map(([key, value]) => [text(key), encodeValue(value, 0, queryVersion === CLOUD_QUERY_VERSION)])) };
  });
}

function validatePrepared(prepared) {
  const fields = prepared?.version === 1 ? 'body,databaseId,url,version' : 'body,databaseId,queryVersion,url,version';
  if (!plainObject(prepared) || Object.keys(prepared).sort().join(',') !== fields || ![1, 2].includes(prepared.version) ||
      (prepared.version === 2 && ![2, CLOUD_QUERY_VERSION].includes(prepared.queryVersion)) || typeof prepared.databaseId !== 'string' || !prepared.databaseId ||
      (prepared.url?.startsWith('file:') ? normalizeFileUrl(prepared.url) : normalizeUrl(prepared.url)) !== prepared.url || typeof prepared.body !== 'string') throw new TypeError('invalid saved request');
  checkBodySize(prepared.body, REPORTED_LIMIT_CEILINGS.maxRequestBytes);
  const body = JSON.parse(prepared.body);
  if (!plainObject(body) || Object.keys(body).some(key => !['requestId', 'afterSequence', 'ifSequence', 'statements', 'workLimits'].includes(key)) || typeof body.requestId !== 'string' || !/^[A-Za-z0-9_-]{1,80}$/.test(body.requestId)) throw new TypeError('invalid saved request identity');
  encodeWorkLimits(body.workLimits);
  checkSequence(body.afterSequence);
  if (body.ifSequence !== undefined) {
    checkSequence(body.ifSequence);
    if (body.ifSequence < body.afterSequence) throw new RangeError('invalid saved sequence condition');
  }
  if (!Array.isArray(body.statements) || body.statements.length < 1 || body.statements.length > REPORTED_LIMIT_CEILINGS.maxStatements) throw new TypeError('invalid saved statements');
  for (const statement of body.statements) {
    if (!plainObject(statement) || Object.keys(statement).some(key => !['sql', 'params'].includes(key)) || typeof statement.sql !== 'string' || !statement.sql.trim() || (statement.params !== undefined && !plainObject(statement.params))) throw new TypeError('invalid saved statement');
    for (const value of Object.values(statement.params ?? {})) decodeValue(value);
  }
  return body;
}

function confirmReply(reply, request, reading, prepared, limits) {
  const invalid = () => new ClientError('RDB_PROTOCOL', reading ? 'Read response identity or results differ' : 'Write response could not be confirmed; recover the saved request', reading ? 'read-failed' : 'unknown', prepared ? { request: prepared } : {});
  if (!plainObject(reply) || reply.requestId !== request.requestId || !sequence(reply.sequence, true) || (reading ? reply.readVersion !== 2 || reply.sequence < request.afterSequence : reply.readVersion !== undefined || reply.sequence <= request.afterSequence || reply.sequence - request.afterSequence > 64)) throw invalid();
  if (plainObject(reply.error) && typeof reply.error.code === 'string' && /^RDB_[A-Z0-9_]+$/.test(reply.error.code) && typeof reply.error.message === 'string' && reply.results === undefined) throw new ClientError(reply.error.code, reply.error.message.slice(0, 2048), reading ? 'read-failed' : 'rolled-back', { sequence: reply.sequence, request: prepared });
  if (!Array.isArray(reply.results) || reply.results.length !== request.statements.length || reply.error !== undefined || byteLength(JSON.stringify(reply.results)) > limits.maxResultBytes) throw invalid();
  try {
    const results = reply.results.map(result => {
      if (!plainObject(result) || !Array.isArray(result.columns) || result.columns.some(name => typeof name !== 'string') || !Array.isArray(result.rows) || result.rows.length > limits.maxRows || typeof result.affected !== 'string' || !/^(?:0|[1-9][0-9]{0,19})$/.test(result.affected) || BigInt(result.affected) > (1n << 64n) - 1n) throw invalid();
      return { columns: result.columns, rows: result.rows.map(row => {
        if (!Array.isArray(row) || row.length !== result.columns.length) throw invalid();
        return row.map(value => decodeValue(value));
      }), affected: BigInt(result.affected) };
    });
    return { requestId: reply.requestId, sequence: reply.sequence, results, ...(reading ? { readVersion: 2 } : {}), ...(reply.executionWork === undefined ? {} : { executionWork: reply.executionWork }) };
  } catch { throw invalid(); }
}

async function boundedJson(response, limit) {
  if (!/^application\/json(?:\s*;|$)/i.test(response.headers.get('content-type') ?? '')) throw new TypeError('response requires JSON');
  if (!response.body) throw new TypeError('response body is missing');
  const reader = response.body.getReader();
  const chunks = [];
  let length = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > limit) throw new RangeError('response exceeds client byte limit');
      chunks.push(value);
    }
    const bytes = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
  } finally { await reader.cancel().catch(() => {}); }
}

// Passes the download through and fails it unless exactly `size` bytes arrive.
function countedBody(body, size, controller, finish) {
  const reader = body.getReader();
  let received = 0;
  const failure = (message, cause) => new ClientError(controller.signal.aborted ? 'RDB_CANCELLED' : 'RDB_TRANSPORT', message, 'read-failed', cause === undefined ? {} : { cause });
  return new ReadableStream({
    async pull(stream) {
      let chunk;
      try { chunk = await reader.read(); }
      catch (error) { finish(); stream.error(failure('Database file download failed', error)); return; }
      if (chunk.done) {
        finish();
        if (received === size) stream.close();
        else stream.error(failure('Database file ended before its declared size'));
        return;
      }
      received += chunk.value.byteLength;
      if (received > size) {
        finish(); controller.abort();
        stream.error(new ClientError('RDB_PROTOCOL', 'Database file is larger than its declared size', 'read-failed'));
        return;
      }
      stream.enqueue(chunk.value);
    },
    cancel(reason) { finish(); controller.abort(); return reader.cancel(reason).catch(() => {}); },
  }, { highWaterMark: 0 });
}

function loopbackHost(hostname) { return ['127.0.0.1', 'localhost', '[::1]'].includes(hostname); }

function capabilities(url, info, local) {
  if (local) {
    if ((url.startsWith('file:') ? info.adapterVersion : info.serviceVersion) !== 1 || !plainObject(info.capabilities) || !plainObject(info.limits) || info.capabilities.readVersion !== 2 || typeof info.capabilities.atomicWrites !== 'boolean' || typeof info.capabilities.exactSequenceCondition !== 'boolean' || (info.capabilities.atomicWrites && info.capabilities.replayWindow !== 64)) throw new ClientError('RDB_PROTOCOL', 'Unsupported database client contract', 'read-failed');
    return { ...info.capabilities, provenance: 'negotiated' };
  }
  return { provenance: 'documented', readVersion: 2, atomicWrites: true, replayWindow: 64, exactSequenceCondition: true, negotiatedLimits: false };
}

function requestLimits(reported, fallback) {
  if (reported === undefined || reported === null) return fallback;
  if (!plainObject(reported)) throw new ClientError('RDB_PROTOCOL', 'Invalid database limits', 'read-failed');
  const checked = (name, limit) => {
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > REPORTED_LIMIT_CEILINGS[name]) throw new ClientError('RDB_PROTOCOL', `Invalid database limit ${name}`, 'read-failed');
    return limit;
  };
  const limits = Object.fromEntries(Object.entries(fallback).map(([name, value]) => [name, checked(name, reported[name] ?? value)]));
  for (const name of COMMIT_LIMITS) if (!Object.hasOwn(limits, name) && reported[name] !== undefined) limits[name] = checked(name, reported[name]);
  return Object.freeze(limits);
}

function sequence(value, reply = false) { return Number.isSafeInteger(value) && value >= 0 && value <= (reply ? MAX_SEQUENCE + 1 : MAX_SEQUENCE); }
function encodeWorkLimits(value) {
  if (value === undefined) return undefined;
  const ceilings = { rowsRead: 1_000_000, rowMutations: 100_000, vmSteps: 10_000_000 };
  if (!plainObject(value)) throw new TypeError('workLimits require an object');
  for (const [name, limit] of Object.entries(value)) {
    if (!Object.hasOwn(ceilings, name) || !Number.isSafeInteger(limit) || limit < 0 || limit > ceilings[name]) throw new RangeError('workLimits require nonnegative integers within the service ceilings');
  }
  return Object.fromEntries(Object.keys(ceilings).filter(key => Object.hasOwn(value, key)).map(key => [key, value[key]]));
}
function checkSequence(value) { if (!sequence(value)) throw new RangeError('sequence requires a nonnegative safe integer below Number.MAX_SAFE_INTEGER'); }
const MAX_EXPORT_WAIT = 3 * 3600_000, EXPORT_POLL_MS = 2000, EXPORT_POLL_FAILURES = 10, CANCEL_MS = 10_000;
function timeout(value) { if (!Number.isInteger(value) || value < 1 || value > MAX_TIMEOUT_MS) throw new RangeError(`timeoutMs must be 1..${MAX_TIMEOUT_MS}`); return value; }
function byteLength(text) { return encoder.encode(text).byteLength; }
function checkBodySize(body, limit) {
  if (byteLength(body) > limit) throw new RangeError(`request exceeds ${byteSize(limit)}`);
}
function byteSize(bytes) {
  return bytes % 1_048_576 === 0 ? `${bytes / 1_048_576} MiB` : bytes % 1024 === 0 ? `${bytes / 1024} KiB` : `${bytes} bytes`;
}
function preparedQueryVersion(prepared) { return prepared.version === 1 ? 2 : prepared.queryVersion; }
// Cloud refuses a queued request with 503 query_queue_* before dispatching it,
// so nothing ran and one retry is safe for reads and writes alike.
async function queueRefused(response) {
  if (response.status !== 503) return false;
  try { return /^query_queue_[a-z_]{1,40}$/.test((await response.clone().json())?.code ?? ''); } catch { return false; }
}
function queueRetryDelay(response) {
  const header = response.headers.get('retry-after');
  return header === null ? 0 : Math.min(retryAfter(header) ?? 1000, 2000);
}
// Retry-After holds delay seconds or an HTTP date.
function retryAfter(value) {
  if (value === null) return undefined;
  if (/^\s*\d{1,9}(?:\.\d+)?\s*$/.test(value)) return Math.round(Number(value) * 1000);
  const date = /GMT\s*$/.test(value) ? Date.parse(value) : NaN;
  return Number.isNaN(date) ? undefined : Math.max(0, date - Date.now());
}
function pause(ms, signal) {
  return new Promise((resolve, reject) => {
    if (signal.aborted) { reject(signal.reason); return; }
    const timer = setTimeout(() => { signal.removeEventListener('abort', stop); resolve(); }, ms);
    const stop = () => { clearTimeout(timer); reject(signal.reason); };
    signal.addEventListener('abort', stop, { once: true });
  });
}
function fileQuery({ emptyTables, newIdentity }) {
  const parts = [];
  if (emptyTables?.length) parts.push(`empty=${emptyTables.map(encodeURIComponent).join(',')}`);
  if (newIdentity) parts.push('identity=new');
  return parts.length ? `?${parts.join('&')}` : '';
}
function httpCode(status) { return status === 401 ? 'RDB_AUTH' : status === 403 ? 'RDB_FORBIDDEN' : status === 409 ? 'RDB_CONFLICT' : status === 429 || status === 413 ? 'RDB_LIMIT' : 'RDB_REMOTE'; }
function redactMessage(message, token) { return (token ? message.replaceAll(token, '[redacted]') : message).slice(0, 2048); }
function safeMessage(reply, token) { return redactMessage(typeof reply?.error === 'string' ? reply.error : typeof reply?.error?.message === 'string' ? reply.error.message : 'Database request was rejected', token); }

async function saveRequest(path, request) { return require('./request-journal.cjs').saveRequest(path, request, validatePrepared); }
async function loadRequest(path) { return require('./request-journal.cjs').loadRequest(path, validatePrepared); }

module.exports = { connect, RemoteDatabase, ClientError, Float64, Record, Vector, encodeValue, decodeValue, normalizeUrl, validatePrepared, saveRequest, loadRequest };
