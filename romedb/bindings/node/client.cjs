'use strict';
const { randomUUID } = require('node:crypto');
const { Buffer } = require('node:buffer');
const { addAbortListener } = require('node:events');
const { Float64, Record, Vector, encodeValue, decodeValue, plainObject, text } = require('./values.cjs');

const MAX_REQUEST = 65_536;
const MAX_CLOUD_REQUEST = 1_048_576;
const CLOUD_QUERY_VERSION = 3;
const MAX_RESPONSE = 524_288;
const MAX_SEQUENCE = Number.MAX_SAFE_INTEGER - 1;
const construction = Symbol('RemoteDatabase');
const warmAtConnect = Symbol('warmAtConnect');
const WARM_STATES = new Set(['ready', 'starting', 'skipped']);
const { pathToFileURL, fileURLToPath } = require('node:url');

class ClientError extends Error {
  constructor(code, message, outcome, details = {}) {
    super(message);
    this.name = 'ClientError';
    this.code = code;
    this.outcome = outcome;
    Object.assign(this, details);
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
  #url; #token; #fetch; #timeout; #sequence = 0; #id; #capabilities; #transport;
  #closed = false; #active = new Set(); #inflightBytes = 0; #sequenceKnown = false; #deferred = false; #cloud = false; #warm = false;
  constructor(options, key, transport) {
    if (key !== construction) throw new TypeError('use connect()');
    if (!plainObject(options)) throw new TypeError('connection options require an object');
    this.#url = transport ? normalizeFileUrl(options.url) : normalizeUrl(options.url);
    this.#transport = transport;
    if (typeof options.token !== 'string' || !/^[\x21-\x7e]{1,4096}$/.test(options.token)) throw new TypeError('token requires a bounded bearer credential');
    this.#token = options.token;
    this.#fetch = options.fetch ?? globalThis.fetch;
    if (typeof this.#fetch !== 'function') throw new TypeError('fetch must be a function');
    this.#timeout = timeout(options.timeoutMs ?? 20_000);
    if (options.deferInfo !== undefined && typeof options.deferInfo !== 'boolean') throw new TypeError('deferInfo requires a boolean');
    if (options.warm !== undefined && typeof options.warm !== 'boolean') throw new TypeError('warm requires a boolean');
    const cloudId = transport ? undefined : /\/databases\/([0-9a-f-]{36})$/.exec(this.#url)?.[1];
    this.#cloud = cloudId !== undefined;
    if (!this.#cloud && options.deferInfo === true) throw new TypeError('deferInfo applies only to Cloud database URLs');
    if (!this.#cloud && options.warm === true) throw new TypeError('warm applies only to Cloud database URLs');
    // Cloud database URLs carry the database id and have documented capabilities,
    // so the details request waits until a write needs a current sequence.
    if (this.#cloud && options.deferInfo !== false) {
      this.#id = cloudId; this.#capabilities = capabilities(this.#url, {}); this.#deferred = true;
    }
    this.#warm = this.#cloud && options.warm !== false;
  }
  [warmAtConnect]() {
    if (this.#warm) this.warm().catch(() => {});
  }
  get deferred() { return this.#deferred; }
  get url() { return this.#url; }
  get sequence() { return this.#sequence; }
  get databaseId() { return this.#id; }
  async info(options = {}) {
    const info = await this.#request('', undefined, options, false);
    if (!plainObject(info) || typeof info.id !== 'string' || !/^[A-Za-z0-9_-]{1,80}$/.test(info.id) || !sequence(info.sequence, true) || (this.#id !== undefined && this.#id !== info.id)) {
      throw new ClientError('FDB_PROTOCOL', 'Invalid database details or changed database identity', 'read-failed');
    }
    const supported = capabilities(this.#url, info);
    this.#id = info.id;
    this.#sequence = Math.max(this.#sequence, info.sequence);
    this.#sequenceKnown = true;
    this.#capabilities = supported;
    return { ...info, capabilities: supported };
  }
  async warm(options = {}) {
    this.#assertOpen();
    if (!this.#cloud) throw new ClientError('FDB_UNSUPPORTED', 'warm applies only to Cloud database URLs', 'not-sent');
    const reply = await this.#request('/warm', '{}', options, false);
    if (!plainObject(reply) || !WARM_STATES.has(reply.state)) throw new ClientError('FDB_PROTOCOL', 'Invalid warm reply', 'read-failed');
    return { state: reply.state };
  }
  async read(statements, options = {}) {
    this.#assertOpen();
    if (this.#capabilities.readVersion !== 2) throw new ClientError('FDB_UNSUPPORTED', 'Database does not support fresh reads', 'not-sent');
    if (options.ifSequence !== undefined) throw new TypeError('fresh reads do not accept ifSequence');
    const afterSequence = options.afterSequence ?? this.#sequence;
    checkSequence(afterSequence);
    const workLimits = encodeWorkLimits(options.workLimits);
    const queryVersion = this.#queryVersion();
    const body = JSON.stringify({ readVersion: 2, requestId: randomUUID(), afterSequence, statements: encodeStatements(statements, queryVersion), ...(workLimits === undefined ? {} : { workLimits }) });
    checkBodySize(body, queryVersion);
    const request = JSON.parse(body);
    const reply = await this.#request('/read', body, options, false, undefined, queryVersion);
    const result = confirmReply(reply, request, true);
    this.#sequence = Math.max(this.#sequence, result.sequence);
    this.#sequenceKnown = true;
    return result;
  }
  prepareWrite(statements, options = {}) {
    this.#assertOpen();
    if (!this.#sequenceKnown && this.#deferred && options.afterSequence === undefined) throw new ClientError('FDB_SEQUENCE_UNKNOWN', 'Read, call info() or pass afterSequence before preparing a write on a deferred connection', 'not-sent');
    if (this.#capabilities.atomicWrites !== true || this.#capabilities.replayWindow !== 64) throw new ClientError('FDB_UNSUPPORTED', 'Database does not support atomic writes with recovery', 'not-sent');
    const afterSequence = options.afterSequence ?? this.#sequence;
    checkSequence(afterSequence);
    if (options.ifSequence !== undefined) {
      if (this.#capabilities.exactSequenceCondition !== true) throw new ClientError('FDB_UNSUPPORTED', 'Database does not support exact sequence conditions', 'not-sent');
      checkSequence(options.ifSequence);
      if (options.ifSequence < afterSequence) throw new RangeError('ifSequence must be at least afterSequence');
    }
    const requestId = options.requestId ?? randomUUID();
    if (typeof requestId !== 'string' || !/^[A-Za-z0-9_-]{1,80}$/.test(requestId)) throw new TypeError('requestId requires 1..80 letters, digits, underscores or hyphens');
    const workLimits = encodeWorkLimits(options.workLimits);
    const queryVersion = this.#queryVersion();
    const body = JSON.stringify({ requestId, afterSequence, ...(options.ifSequence === undefined ? {} : { ifSequence: options.ifSequence }), statements: encodeStatements(statements, queryVersion), ...(workLimits === undefined ? {} : { workLimits }) });
    checkBodySize(body, queryVersion);
    return Object.freeze({ version: 2, url: this.#url, databaseId: this.#id, queryVersion, body });
  }
  async submit(prepared, options = {}) {
    this.#assertOpen();
    const request = validatePrepared(prepared);
    if (this.#capabilities.atomicWrites !== true || this.#capabilities.replayWindow !== 64 || (request.ifSequence !== undefined && this.#capabilities.exactSequenceCondition !== true)) throw new ClientError('FDB_UNSUPPORTED', 'Database does not support this write contract', 'not-sent', { request: prepared });
    if (prepared.url !== this.#url || prepared.databaseId !== this.#id) throw new ClientError('FDB_REQUEST_IDENTITY', 'Saved request belongs to another database', 'not-sent');
    try {
      const reply = await this.#request('/query', prepared.body, options, true, prepared, preparedQueryVersion(prepared));
      const result = confirmReply(reply, request, false, prepared);
      this.#sequence = Math.max(this.#sequence, result.sequence);
      this.#sequenceKnown = true;
      return result;
    } catch (error) {
      if (error instanceof ClientError) {
        if (error.outcome === 'rolled-back') this.#sequence = Math.max(this.#sequence, error.sequence);
        throw error;
      }
      throw new ClientError('FDB_PROTOCOL', 'Write response could not be confirmed; recover the saved request', 'unknown', { request: prepared });
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
  async close() {
    this.#closed = true;
    for (const controller of this.#active) controller.abort();
    this.#token = '';
  }
  #assertOpen() {
    if (this.#closed) throw new ClientError('FDB_CLOSED', 'Database client is closed', 'not-sent');
  }
  #queryVersion() { return this.#cloud ? CLOUD_QUERY_VERSION : 2; }
  async #request(path, body, options, writing, prepared, queryVersion = 2) {
    this.#assertOpen();
    const duration = timeout(options.timeoutMs ?? this.#timeout);
    if (options.signal !== undefined && !(options.signal instanceof AbortSignal)) throw new TypeError('signal must be an AbortSignal');
    if (options.signal?.aborted) throw new ClientError('FDB_CANCELLED', 'Request cancelled before dispatch', 'not-sent', prepared ? { request: prepared } : {});
    const bodyBytes = body === undefined ? 0 : Buffer.byteLength(body);
    if (this.#active.size >= 32 || this.#inflightBytes + bodyBytes > 2_097_152) throw new ClientError('FDB_LIMIT', 'Client request limit reached', 'not-sent', prepared ? { request: prepared } : {});
    const controller = new AbortController();
    const listener = options.signal === undefined ? undefined : addAbortListener(options.signal, () => controller.abort());
    const timer = setTimeout(() => controller.abort(), duration);
    this.#active.add(controller);
    this.#inflightBytes += bodyBytes;
    const credential = this.#token;
    let status;
    try {
      if (this.#transport) return await this.#transport(path, body, controller.signal, duration);
      const send = () => this.#fetch(this.#url + path, {
        method: body === undefined ? 'GET' : 'POST',
        headers: { authorization: `Bearer ${credential}`, accept: 'application/json', ...(body === undefined ? {} : { 'content-type': 'application/json' }), ...(this.#id === undefined ? {} : { 'x-romedb-database-id': prepared?.databaseId ?? this.#id }),
          ...(queryVersion === CLOUD_QUERY_VERSION && (path === '/query' || path === '/read') ? { 'x-romedb-query-version': String(queryVersion) } : {}) },
        ...(body === undefined ? {} : { body }),
        redirect: 'error', signal: controller.signal,
      });
      let response = await send();
      if (await queueRefused(response)) {
        await pause(queueRetryDelay(response), controller.signal);
        response = await send();
      }
      status = response.status;
      const replyVersion = response.headers.get('x-romedb-query-version');
      if ((path === '/query' || path === '/read') && replyVersion !== null && replyVersion !== '2' && replyVersion !== String(queryVersion)) {
        await response.body?.cancel();
        throw new ClientError('FDB_PROTOCOL', 'Unsupported database query response version', writing ? 'unknown' : 'read-failed', { status, ...(prepared ? { request: prepared } : {}) });
      }
      const reply = await boundedJson(response, MAX_RESPONSE);
      if (plainObject(reply?.error) && typeof reply.error.message === 'string') reply.error.message = redactMessage(reply.error.message, credential);
      if (!response.ok && !(writing && reply?.requestId && reply?.sequence !== undefined)) {
        const code = plainObject(reply?.error) && typeof reply.error.code === 'string' && /^FDB_[A-Z0-9_]{1,80}$/.test(reply.error.code) ? reply.error.code : httpCode(status);
        throw new ClientError(code, safeMessage(reply, credential), writing ? 'unknown' : 'read-failed', { status, ...(prepared ? { request: prepared } : {}) });
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
          ...(prepared ? { request: prepared } : {}),
        });
      }
      const code = controller.signal.aborted ? 'FDB_CANCELLED' : status === undefined ? 'FDB_TRANSPORT' : 'FDB_PROTOCOL';
      throw new ClientError(code, writing ? 'Write was not confirmed; recover the saved request' : 'Database request failed', writing ? 'unknown' : 'read-failed', { ...(status === undefined ? {} : { status }), ...(prepared ? { request: prepared } : {}) });
    } finally {
      clearTimeout(timer);
      listener?.[Symbol.dispose]();
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
  if (url.protocol !== 'file:' || url.hostname || url.search || url.hash) throw new TypeError('use a local file URL without a query or fragment');
  return pathToFileURL(fileURLToPath(url)).href;
}

function normalizeUrl(input) {
  if (typeof input !== 'string' || input.length > 4096) throw new TypeError('url requires a bounded database URL');
  // romedb:// is an alias for https://; the same checks apply after the rewrite.
  const url = new URL(input.startsWith('romedb://') ? 'https://' + input.slice('romedb://'.length) : input);
  const loopback = ['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname);
  if (url.username || url.password || url.search || url.hash || (url.protocol !== 'https:' && !(url.protocol === 'http:' && loopback))) throw new TypeError('use HTTPS or loopback HTTP without URL credentials, query or fragment');
  let path = url.pathname.replace(/\/$/, '').replace(/\/(?:query|read)$/, '');
  if (loopback && !path) path = '/v1/database';
  const uuid = '[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}';
  if (!(loopback && path === '/v1/database') && !new RegExp(`^/v1/organizations/${uuid}/databases/${uuid}$`).test(path)) throw new TypeError('use a local service URL or stable organization-scoped Cloud database URL');
  return url.origin + path;
}

function encodeStatements(statements, queryVersion = 2) {
  if (!Array.isArray(statements) || statements.length < 1 || statements.length > 32) throw new RangeError('request requires 1..32 statements');
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
  checkBodySize(prepared.body, preparedQueryVersion(prepared));
  const body = JSON.parse(prepared.body);
  if (!plainObject(body) || Object.keys(body).some(key => !['requestId', 'afterSequence', 'ifSequence', 'statements', 'workLimits'].includes(key)) || typeof body.requestId !== 'string' || !/^[A-Za-z0-9_-]{1,80}$/.test(body.requestId)) throw new TypeError('invalid saved request identity');
  encodeWorkLimits(body.workLimits);
  checkSequence(body.afterSequence);
  if (body.ifSequence !== undefined) {
    checkSequence(body.ifSequence);
    if (body.ifSequence < body.afterSequence) throw new RangeError('invalid saved sequence condition');
  }
  if (!Array.isArray(body.statements) || body.statements.length < 1 || body.statements.length > 32) throw new TypeError('invalid saved statements');
  for (const statement of body.statements) {
    if (!plainObject(statement) || Object.keys(statement).some(key => !['sql', 'params'].includes(key)) || typeof statement.sql !== 'string' || !statement.sql.trim() || (statement.params !== undefined && !plainObject(statement.params))) throw new TypeError('invalid saved statement');
    for (const value of Object.values(statement.params ?? {})) decodeValue(value);
  }
  return body;
}

function confirmReply(reply, request, reading, prepared) {
  const invalid = () => new ClientError('FDB_PROTOCOL', reading ? 'Read response identity or results differ' : 'Write response could not be confirmed; recover the saved request', reading ? 'read-failed' : 'unknown', prepared ? { request: prepared } : {});
  if (!plainObject(reply) || reply.requestId !== request.requestId || !sequence(reply.sequence, true) || (reading ? reply.readVersion !== 2 || reply.sequence < request.afterSequence : reply.readVersion !== undefined || reply.sequence <= request.afterSequence || reply.sequence - request.afterSequence > 64)) throw invalid();
  if (plainObject(reply.error) && typeof reply.error.code === 'string' && /^FDB_[A-Z0-9_]+$/.test(reply.error.code) && typeof reply.error.message === 'string' && reply.results === undefined) throw new ClientError(reply.error.code, reply.error.message.slice(0, 2048), reading ? 'read-failed' : 'rolled-back', { sequence: reply.sequence, request: prepared });
  if (!Array.isArray(reply.results) || reply.results.length !== request.statements.length || reply.error !== undefined || Buffer.byteLength(JSON.stringify(reply.results)) > 262_144) throw invalid();
  try {
    const results = reply.results.map(result => {
      if (!plainObject(result) || !Array.isArray(result.columns) || result.columns.some(name => typeof name !== 'string') || !Array.isArray(result.rows) || result.rows.length > 1000 || typeof result.affected !== 'string' || !/^(?:0|[1-9][0-9]{0,19})$/.test(result.affected) || BigInt(result.affected) > (1n << 64n) - 1n) throw invalid();
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
    return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(chunks)));
  } finally { await reader.cancel().catch(() => {}); }
}

function capabilities(url, info) {
  if (url.startsWith('file:') || url.endsWith('/v1/database')) {
    if ((url.startsWith('file:') ? info.adapterVersion : info.serviceVersion) !== 1 || !plainObject(info.capabilities) || !plainObject(info.limits) || info.capabilities.readVersion !== 2 || typeof info.capabilities.atomicWrites !== 'boolean' || typeof info.capabilities.exactSequenceCondition !== 'boolean' || (info.capabilities.atomicWrites && info.capabilities.replayWindow !== 64)) throw new ClientError('FDB_PROTOCOL', 'Unsupported database client contract', 'read-failed');
    return { ...info.capabilities, provenance: 'negotiated' };
  }
  return { provenance: 'documented', readVersion: 2, atomicWrites: true, replayWindow: 64, exactSequenceCondition: true, negotiatedLimits: false };
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
function timeout(value) { if (!Number.isInteger(value) || value < 1 || value > 120_000) throw new RangeError('timeoutMs must be 1..120000'); return value; }
function checkBodySize(body, queryVersion = 2) {
  if (queryVersion === CLOUD_QUERY_VERSION ? Buffer.byteLength(body) > MAX_CLOUD_REQUEST : Buffer.byteLength(body) > MAX_REQUEST)
    throw new RangeError(queryVersion === CLOUD_QUERY_VERSION ? 'request exceeds 1 MiB' : 'request exceeds 64 KiB');
}
function preparedQueryVersion(prepared) { return prepared.version === 1 ? 2 : prepared.queryVersion; }
// Cloud refuses a queued request with 503 query_queue_* before dispatching it,
// so nothing ran and one retry is safe for reads and writes alike.
async function queueRefused(response) {
  if (response.status !== 503) return false;
  try { return /^query_queue_[a-z_]{1,40}$/.test((await response.clone().json())?.code ?? ''); } catch { return false; }
}
function queueRetryDelay(response) {
  const seconds = Number(response.headers.get('retry-after'));
  return Number.isFinite(seconds) && seconds >= 0 ? Math.min(seconds * 1000, 2000) : 1000;
}
function pause(ms, signal) {
  return new Promise((resolve, reject) => {
    if (signal.aborted) { reject(signal.reason); return; }
    const timer = setTimeout(() => { signal.removeEventListener('abort', stop); resolve(); }, ms);
    const stop = () => { clearTimeout(timer); reject(signal.reason); };
    signal.addEventListener('abort', stop, { once: true });
  });
}
function httpCode(status) { return status === 401 ? 'FDB_AUTH' : status === 403 ? 'FDB_FORBIDDEN' : status === 409 ? 'FDB_CONFLICT' : status === 429 || status === 413 ? 'FDB_LIMIT' : 'FDB_REMOTE'; }
function redactMessage(message, token) { return (token ? message.replaceAll(token, '[redacted]') : message).slice(0, 2048); }
function safeMessage(reply, token) { return redactMessage(typeof reply?.error === 'string' ? reply.error : typeof reply?.error?.message === 'string' ? reply.error.message : 'Database request was rejected', token); }

async function saveRequest(path, request) { return require('./request-journal.cjs').saveRequest(path, request, validatePrepared); }
async function loadRequest(path) { return require('./request-journal.cjs').loadRequest(path, validatePrepared); }

module.exports = { connect, RemoteDatabase, ClientError, Float64, Record, Vector, encodeValue, decodeValue, normalizeUrl, validatePrepared, saveRequest, loadRequest };
