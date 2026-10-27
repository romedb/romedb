'use strict';
const { NativeDatabase, interruptConnection, createCancellationToken, cancelOperation, releaseCancellationToken, vectorFromComponents, vectorFromSparseEntries } = require('./native.cjs');
class Record {
  constructor(table, key) {
    if (typeof table !== 'string' || !['string','bigint'].includes(typeof key)) throw new TypeError('Record requires a table and string or bigint key');
    this.table = table; this.key = key;
    Object.freeze(this);
  }
}
class Vector {
  static float32(values) { return constructVector('float32', values); }
  static float64(values) { return constructVector('float64', values); }
  static sparse32(values) { return constructVector('sparse32', values); }
  static sparse32Entries(dimensions, entries) {
    if (!Number.isInteger(dimensions) || dimensions < 1 || dimensions > 65536) throw new RangeError('vector dimensions must be 1..65536');
    if (!Array.isArray(entries)) throw new TypeError('sparse entries require an array of index/value pairs');
    if (entries.length > dimensions) throw new RangeError('sparse entry count exceeds dimensions');
    const bytes = Buffer.alloc(entries.length * 12);
    let previous = -1;
    for (let i = 0; i < entries.length; i++) {
      const entry = entries[i];
      if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError('sparse entries require index/value pairs');
      const [index, value] = entry;
      if (!Number.isInteger(index) || index <= previous || index >= dimensions) throw new RangeError('sparse indices must be increasing and within dimensions');
      if (typeof value !== 'number' || !Number.isFinite(value)) throw new TypeError('vector components must be finite numbers');
      if (!Number.isFinite(Math.fround(value))) throw new RangeError('vector component is outside the finite float32 range');
      bytes.writeUInt32LE(index, i * 12);
      bytes.writeDoubleLE(value, i * 12 + 4);
      previous = index;
    }
    return new Vector(vectorFromSparseEntries(dimensions, bytes));
  }
  static quantized8(values) { return constructVector('quantized8', values); }
  static bit1(values) { return constructVector('bit1', values); }
  constructor(bytes) {
    if (!(bytes instanceof Uint8Array)) throw new TypeError('Vector requires encoded bytes');
    this.bytes = Buffer.from(bytes);
  }
}
function constructVector(encoding, values) {
  if (!Array.isArray(values) && !(values instanceof Float32Array) && !(values instanceof Float64Array)) {
    throw new TypeError('vector components require a number array, Float32Array or Float64Array');
  }
  if (values.length < 1 || values.length > 65536) throw new RangeError('vector dimensions must be 1..65536');
  const bytes = Buffer.alloc(values.length * 8);
  for (let i = 0; i < values.length; i++) {
    const value = values[i];
    if (typeof value !== 'number' || !Number.isFinite(value)) throw new TypeError('vector components must be finite numbers');
    if (encoding !== 'float64' && !Number.isFinite(Math.fround(value))) throw new RangeError('vector component is outside the finite float32 range');
    bytes.writeDoubleLE(value, i * 8);
  }
  return new Vector(vectorFromComponents(encoding, bytes));
}
function encode(value, depth = 0) {
  if (depth > 64) throw new RangeError('value nesting exceeds 64');
  const tagged = (type, value) => ({ type, value });
  if (value === null) return { type: 'Null' };
  if (typeof value === 'boolean') return tagged('Boolean', value);
  if (typeof value === 'string') return tagged('String', text(value));
  if (typeof value === 'bigint') {
    if (value < -(1n << 63n) || value >= (1n << 63n)) throw new RangeError('integer outside int64');
    return tagged('Integer', value.toString());
  }
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) throw new TypeError('non-finite number');
    if (Number.isInteger(value) && !Object.is(value, -0)) {
      if (!Number.isSafeInteger(value)) throw new RangeError('use bigint for integers outside the safe Number range');
      return tagged('Integer', String(value));
    }
    const bytes = Buffer.alloc(8); bytes.writeDoubleBE(value);
    return tagged('Number', bytes.toString('hex'));
  }
  if (value instanceof Record) return tagged('Record', { table: text(value.table), key: encode(value.key, depth) });
  if (value instanceof Vector) return tagged('Vector', [...value.bytes]);
  if (value instanceof Uint8Array) return tagged('Binary', [...value]);
  if (Array.isArray(value)) {
    const entries = [];
    for (let i = 0; i < value.length; i++) {
      if (!Object.hasOwn(value, i)) throw new TypeError('array holes are not database values');
      entries.push(encode(value[i], depth + 1));
    }
    return tagged('Array', entries);
  }
  if (value && (Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null)) {
    return tagged('Object', Object.fromEntries(Object.entries(value).map(([k,v]) => [text(k), encode(v, depth + 1)])));
  }
  throw new TypeError('unsupported parameter value');
}
function text(value) {
  if (!value.isWellFormed()) throw new TypeError('strings must be well-formed Unicode');
  return value;
}
function decode(value) {
  switch (value.type) {
    case 'Null': return null;
    case 'Boolean': case 'String': return value.value;
    case 'Integer': return BigInt(value.value);
    case 'Number': return Buffer.from(value.value, 'hex').readDoubleBE();
    case 'Binary': return Buffer.from(value.value);
    case 'Vector': return new Vector(Buffer.from(value.value));
    case 'Record': return new Record(value.value.table, decode(value.value.key));
    case 'Array': return value.value.map(decode);
    case 'Object': return Object.fromEntries(Object.entries(value.value).map(([k,v]) => [k, decode(v)]));
    default: throw new Error('unsupported native value encoding');
  }
}
function unwrap(raw) {
  const report = JSON.parse(raw);
  if (report.version !== 1) throw new Error('unsupported native report version');
  if (report.execution.error) {
    const error = new Error(report.execution.error.message);
    error.code = report.execution.error.code;
    error.transaction = report.transaction;
    if (report.execution.error.migration) {
      const migration = report.execution.error.migration;
      error.migration = {version:BigInt(migration.version),offset:BigInt(migration.offset),cause:migration.cause};
    }
    throw error;
  }
  return report;
}
// Parsed wire rows are private to this response. Replacing cells releases their
// wire representation as decoding proceeds instead of retaining a second rowset.
function decodeRows(rows) {
  for (const row of rows) {
    for (let i = 0; i < row.length; i++) row[i] = decode(row[i]);
  }
  return rows;
}
function decodeBatch(raw) {
  return unwrap(raw).execution.result.map(entry => {
    if (entry.result) entry.result = { ...entry.result,
      rows: decodeRows(entry.result.rows), affected: BigInt(entry.result.affected) };
    return entry;
  });
}
function migrationPlan(migrations) {
  const plan = migrations.map(m => {
      if (typeof m.version !== 'bigint' || typeof m.name !== 'string' || typeof m.sql !== 'string') {
        throw new TypeError('migration requires bigint version, string name and SQL');
      }
      return { version: encode(m.version).value, name: m.name, sql: m.sql };
    });
  return plan;
}
function resultLimits(limits) {
  if (limits === null || typeof limits !== 'object' || Array.isArray(limits)) throw new TypeError('result limits must be an object');
  for (const key of Object.keys(limits)) {
    if (key !== 'maxRows' && key !== 'maxPayloadBytes') throw new TypeError(`unknown result limit ${key}`);
  }
  return ['maxRows', 'maxPayloadBytes'].map(key => {
    const value = limits[key];
    if (typeof value !== 'bigint') throw new TypeError('result limits require both bigint fields');
    if (value < 0n || value > 18446744073709551615n) throw new RangeError('result limits must fit uint64');
    return value.toString();
  });
}
function connectionOptions(options) {
  if (options === null || typeof options !== 'object' || Array.isArray(options)) throw new TypeError('database options must be an object');
  for (const key of Object.keys(options)) {
    if (key !== 'writeBufferLimits') throw new TypeError(`unknown database option ${key}`);
  }
  return options.writeBufferLimits === undefined ? [] : resultLimits(options.writeBufferLimits);
}
function integrityLimits(limits) {
  if (limits === null || typeof limits !== 'object' || Array.isArray(limits)) throw new TypeError('integrity limits must be an object');
  const fields = { maxDocuments: '', maxEncodedBytes: '' };
  for (const [key, value] of Object.entries(limits)) {
    if (key !== 'maxDocuments' && key !== 'maxEncodedBytes') throw new TypeError(`unknown integrity limit ${key}`);
    if (value === undefined) continue;
    if (typeof value !== 'bigint') throw new TypeError('integrity limits require bigint');
    if (value < 0n || value > 18446744073709551615n) throw new RangeError('integrity limits must fit uint64');
    fields[key] = value.toString();
  }
  return [fields.maxDocuments, fields.maxEncodedBytes];
}
function decodeIntegrity(raw) {
  const report = unwrap(raw);
  return { ...Object.fromEntries(Object.entries(report.execution.result).map(([key, value]) => [key, BigInt(value)])), transaction: report.transaction };
}
function decodeProfile(raw) {
  const report = unwrap(raw);
  const { result, metrics } = report.execution.result;
  return { result: { columns: result.columns, rows: decodeRows(result.rows),
    affected: BigInt(result.affected), transaction: report.transaction },
    metrics: Object.fromEntries(Object.entries(metrics).map(([key, value]) => [key, BigInt(value)])) };
}
function isRomeDBError(value) {
  if (!(value instanceof Error) || typeof value.code !== 'string' || !/^FDB_[A-Z][A-Z0-9_]*$/.test(value.code)) return false;
  const transaction = value.transaction;
  if (!(transaction === undefined || (transaction !== null && typeof transaction === 'object' &&
    (transaction.before === 'autocommit' || transaction.before === 'active') &&
    (transaction.after === 'autocommit' || transaction.after === 'active')))) return false;
  const migration = value.migration;
  return migration === undefined || (migration !== null && typeof migration === 'object' &&
    typeof migration.version === 'bigint' && migration.version > 0n &&
    typeof migration.offset === 'bigint' && migration.offset >= 0n &&
    migration.cause !== null && typeof migration.cause === 'object' &&
    typeof migration.cause.code === 'string' && /^FDB_[A-Z][A-Z0-9_]*$/.test(migration.cause.code) &&
    typeof migration.cause.message === 'string');
}
exports.isRomeDBError = isRomeDBError;
function cardinalityError(count, transaction) {
  const error = new RangeError(`expected exactly one row, got ${count}`);
  error.code = 'FDB_CARDINALITY';
  error.transaction = transaction;
  return error;
}
function closedError(message = 'database is closed') {
  const error = new Error(message);
  error.code = 'FDB_CLOSED';
  return error;
}
class Database {
  collection(name) { return collectionClient(this, name, false); }
  #handle; #closed = false;
  constructor(path = ':memory:', options = {}) { this.#handle = new NativeDatabase(path, ...connectionOptions(options)); }
  get #native() {
    if (this.#closed) throw closedError();
    return this.#handle;
  }
  close() { this.#handle.close(); this.#closed = true; }
  execute(sql, parameters = {}) {
    const params = Object.fromEntries(Object.entries(parameters).map(([k,v]) => [text(k), encode(v)]));
    const report = unwrap(this.#native.execute(sql, JSON.stringify(params)));
    const result = report.execution.result;
    return { columns: result.columns, rows: decodeRows(result.rows), affected: BigInt(result.affected), transaction: report.transaction };
  }
  profileSelect(sql, parameters = {}) {
    const params = Object.fromEntries(Object.entries(parameters).map(([k,v]) => [text(k), encode(v)]));
    return decodeProfile(this.#native.profileSelect(sql, JSON.stringify(params)));
  }
  selectWithLimits(sql, limits, parameters = {}) {
    return this.profileSelectWithLimits(sql, limits, parameters).result;
  }
  profileSelectWithLimits(sql, limits, parameters = {}) {
    const args = resultLimits(limits);
    const params = Object.fromEntries(Object.entries(parameters).map(([k,v]) => [text(k), encode(v)]));
    return decodeProfile(this.#native.profileSelectWithLimits(sql, JSON.stringify(params), ...args));
  }
  writeWithResultLimits(sql, limits, parameters = {}) {
    const args = resultLimits(limits);
    const params = Object.fromEntries(Object.entries(parameters).map(([k,v]) => [text(k), encode(v)]));
    const report = unwrap(this.#native.writeWithResultLimits(sql, JSON.stringify(params), ...args));
    const result = report.execution.result;
    return { columns: result.columns, rows: decodeRows(result.rows), affected: BigInt(result.affected), transaction: report.transaction };
  }
  checkCollectionIntegrity(table, limits = {}) {
    return decodeIntegrity(this.#native.checkCollectionIntegrity(table, ...integrityLimits(limits)));
  }
  executeBatch(script) { return decodeBatch(this.#native.executeBatch(script)); }
  exportDocuments(table, format = 'json') {
    return unwrap(this.#native.exportDocuments(table, format)).execution.result;
  }
  importDocuments(table, input, format = 'json') {
    const report = unwrap(this.#native.importDocuments(table, input, format));
    return { ...report.execution.result, transaction: report.transaction };
  }
  migrate(migrations) {
    const plan = migrationPlan(migrations);
    const report = unwrap(this.#native.migrate(JSON.stringify(plan)));
    return { alreadyApplied: report.execution.result.alreadyApplied,
      applied: report.execution.result.applied.map(BigInt), transaction: report.transaction };
  }
  all(sql, parameters) { return this.execute(sql, parameters).rows; }
  first(sql, parameters) { return this.all(sql, parameters)[0]; }
  exactlyOne(sql, parameters) {
    const { rows, transaction } = this.execute(sql, parameters);
    if (rows.length !== 1) throw cardinalityError(rows.length, transaction);
    return rows[0];
  }
}
exports.Database = Database;
exports.Record = Record;
exports.Vector = Vector;


const asyncConstruction = Symbol('AsyncDatabase');
class AsyncDatabase {
  collection(name) { return collectionClient(this, name, true); }
  #interruptKey;
  #worker; #pending = new Map(); #next = 0; #bytes = 0;
  #ready; #readyResolve; #readyReject; #exited;
  #failure; #closing = false; #closePromise; #stopped = false; #shutdownRequested = false;
  constructor(path, token, limits) {
    if (token !== asyncConstruction) throw new TypeError('use AsyncDatabase.open()');
    const { Worker } = require('node:worker_threads');
    this.#ready = new Promise((resolve, reject) => { this.#readyResolve = resolve; this.#readyReject = reject; });
    this.#worker = new Worker(require.resolve('./worker.cjs'), { workerData: { path, limits } });
    this.#exited = new Promise(resolve => this.#worker.once('exit', code => {
      this.#stopped = true;
      if (!this.#closing || this.#pending.size) this.#fail(new Error(`database worker exited (${code})`));
      resolve();
    }));
    this.#worker.on('error', error => this.#fail(error));
    this.#worker.on('messageerror', error => this.#fail(error, true));
    this.#worker.on('message', message => {
      if (message.ready) { this.#interruptKey = message.interruptKey; this.#readyResolve(); return; }
      const pending = this.#pending.get(message.id);
      if (!pending) return;
      this.#pending.delete(message.id); this.#bytes -= pending.bytes; pending.cleanup();
      if (message.error) pending.reject(new Error(message.error.message));
      else pending.resolve(message.result);
    });
  }
  interrupt() { return this.#interruptKey !== undefined && interruptConnection(this.#interruptKey); }
  static async open(path = ':memory:', options = {}) {
    const limits = connectionOptions(options);
    const db = new AsyncDatabase(path, asyncConstruction, limits);
    try { await db.#ready; return db; }
    catch (error) { await db.#exited; throw error; }
  }
  #fail(error, shutdown = false) {
    if (!this.#failure) {
      this.#failure = new Error(`database worker failure: ${error.message}`, { cause: error });
      this.#failure.code = 'FDB_WORKER';
    }
    this.#readyReject(this.#failure);
    for (const pending of this.#pending.values()) { pending.cleanup(); pending.reject(this.#failure); }
    this.#pending.clear(); this.#bytes = 0;
    if (shutdown && !this.#stopped && !this.#shutdownRequested) {
      this.#shutdownRequested = true;
      // The response channel is damaged. Ask the worker to close after the
      // accepted queue; their write outcomes can no longer be inferred here.
      try { this.#worker.postMessage({ id: 0, method: 'close', args: [] }); }
      catch { void this.#worker.terminate(); }
    }
  }
  #request(method, args, closing = false, signal, timeoutMs) {
    if (this.#failure) return Promise.reject(this.#failure);
    if (this.#closing && !closing) return Promise.reject(closedError('database is closing or closed'));
    const bytes = args.reduce((size, arg) => size + Buffer.byteLength(arg), 0);
    if (!closing && (this.#pending.size >= 256 || this.#bytes + bytes > 128 * 1024 * 1024)) {
      const error = new RangeError('database worker queue limit exceeded'); error.code = 'FDB_LIMIT';
      return Promise.reject(error);
    }
    const id = ++this.#next;
    return new Promise((resolve, reject) => {
      let cancellationKey, listener;
      const cleanup = () => {
        listener?.[Symbol.dispose]();
        if (cancellationKey !== undefined) releaseCancellationToken(cancellationKey);
      };
      try {
        if (timeoutMs !== undefined && (!Number.isInteger(timeoutMs) || timeoutMs < 0 || timeoutMs > 4294967295)) {
          throw new RangeError('timeoutMs must be an integer from 0 through 4294967295');
        }
        if (signal !== undefined || timeoutMs !== undefined) {
          cancellationKey = createCancellationToken(timeoutMs === undefined ? undefined : String(timeoutMs));
        }
        if (signal !== undefined) {
          listener = require('node:events').addAbortListener(signal, () => cancelOperation(cancellationKey));
          if (signal.aborted) cancelOperation(cancellationKey);
        }
      } catch (error) { cleanup(); reject(error); return; }
      this.#pending.set(id, { resolve, reject, bytes, cleanup }); this.#bytes += bytes;
      try { this.#worker.postMessage({ id, method, args, cancellationKey }); }
      catch (error) { this.#pending.delete(id); this.#bytes -= bytes; cleanup(); reject(error); }
    });
  }
  close() {
    if (!this.#closePromise) {
      this.#closing = true;
      this.#closePromise = this.#failure ? this.#exited : this.#request('close', [], true)
        .then(() => this.#exited)
        .catch(async error => {
          this.#fail(error, true);
          await this.#exited;
          throw this.#failure;
        });
    }
    return this.#closePromise;
  }
  async execute(sql, parameters = {}, options = {}) {
    const params = Object.fromEntries(Object.entries(parameters).map(([k,v]) => [text(k), encode(v)]));
    const report = unwrap(await this.#request('execute', [sql, JSON.stringify(params)], false, options.signal, options.timeoutMs));
    const result = report.execution.result;
    return { columns: result.columns, rows: decodeRows(result.rows), affected: BigInt(result.affected), transaction: report.transaction };
  }
  async profileSelect(sql, parameters = {}, options = {}) {
    const params = Object.fromEntries(Object.entries(parameters).map(([k,v]) => [text(k), encode(v)]));
    return decodeProfile(await this.#request('profileSelect', [sql, JSON.stringify(params)], false, options.signal, options.timeoutMs));
  }
  async selectWithLimits(sql, limits, parameters = {}, options = {}) {
    return (await this.profileSelectWithLimits(sql, limits, parameters, options)).result;
  }
  async profileSelectWithLimits(sql, limits, parameters = {}, options = {}) {
    const args = resultLimits(limits);
    const params = Object.fromEntries(Object.entries(parameters).map(([k,v]) => [text(k), encode(v)]));
    return decodeProfile(await this.#request('profileSelectWithLimits', [sql, JSON.stringify(params), ...args], false, options.signal, options.timeoutMs));
  }
  async writeWithResultLimits(sql, limits, parameters = {}, options = {}) {
    const args = resultLimits(limits);
    const params = Object.fromEntries(Object.entries(parameters).map(([k,v]) => [text(k), encode(v)]));
    const report = unwrap(await this.#request('writeWithResultLimits', [sql, JSON.stringify(params), ...args], false, options.signal, options.timeoutMs));
    const result = report.execution.result;
    return { columns: result.columns, rows: decodeRows(result.rows), affected: BigInt(result.affected), transaction: report.transaction };
  }
  async checkCollectionIntegrity(table, limits = {}, options = {}) {
    return decodeIntegrity(await this.#request('checkCollectionIntegrity', [table, ...integrityLimits(limits)], false, options.signal, options.timeoutMs));
  }
  async executeBatch(script, options = {}) { return decodeBatch(await this.#request('executeBatch', [script], false, options.signal, options.timeoutMs)); }
  async exportDocuments(table, format = 'json', options = {}) {
    return unwrap(await this.#request('exportDocuments', [table, format], false, options.signal, options.timeoutMs)).execution.result;
  }
  async importDocuments(table, input, format = 'json', options = {}) {
    const report = unwrap(await this.#request('importDocuments', [table, input, format], false, options.signal, options.timeoutMs));
    return { ...report.execution.result, transaction: report.transaction };
  }
  async migrate(migrations, options = {}) {
    const report = unwrap(await this.#request('migrate', [JSON.stringify(migrationPlan(migrations))], false, options.signal, options.timeoutMs));
    return { alreadyApplied: report.execution.result.alreadyApplied,
      applied: report.execution.result.applied.map(BigInt), transaction: report.transaction };
  }
  async all(sql, parameters, options) { return (await this.execute(sql, parameters, options)).rows; }
  async first(sql, parameters, options) { return (await this.all(sql, parameters, options))[0]; }
  async exactlyOne(sql, parameters, options) {
    const { rows, transaction } = await this.execute(sql, parameters, options);
    if (rows.length !== 1) throw cardinalityError(rows.length, transaction);
    return rows[0];
  }
}
exports.AsyncDatabase = AsyncDatabase;

// Collection identifiers are quoted; document values and record keys are bound.
function collectionClient(db, name, asynchronous) {
  if (typeof name !== 'string' || !name.length || name.includes('\0')) throw new TypeError('collection name must be a nonempty string without NUL');
  const quote = value => '"' + value.replaceAll('"', '""') + '"';
  const table = quote(name);
  const record = key => new Record(name, key);
  const document = value => {
    if (!value || ![Object.prototype, null].includes(Object.getPrototypeOf(value))) throw new TypeError('expected a document object');
    return value;
  };
  const fields = value => {
    document(value);
    if (Object.hasOwn(value, 'id')) throw new TypeError('pass the record key separately; document must omit id');
    return Object.entries(value);
  };
  const run = (sql, params, options, many = false) => {
    const project = result => many ? result.rows.map(row => row[0]) : result.rows[0]?.[0];
    const check = info => {
      if (info.rows[0]?.[0]?.model !== 'document') throw new TypeError('collection methods require a document collection');
    };
    const missing = error => { if (error.code !== 'FDB_NOT_FOUND') throw error; };
    if (asynchronous) return (async () => {
      try { check(await db.execute(`INFO FOR TABLE ${table}`, {}, options)); } catch (error) { missing(error); }
      return project(await db.execute(sql, params, options));
    })();
    try { check(db.execute(`INFO FOR TABLE ${table}`)); } catch (error) { missing(error); }
    return project(db.execute(sql, params));
  };
  return Object.freeze({
    all(options) { return run(`SELECT * FROM ${table}`, {}, options, true); },
    get(key, options) { return run(`SELECT * FROM ${table} WHERE id=$id`, {$id:record(key)}, options); },
    insert(value, options) { return run(`INSERT INTO ${table} DOCUMENT $doc RETURNING *`, {$doc:document(value)}, options); },
    upsert(key, value, options) {
      const params = {$id:record(key)};
      const body = fields(value).map(([field, v], i) => { params[`$v${i}`] = v; return `${quote(field)}:$v${i}`; });
      return run(`UPSERT ${table} {id:$id${body.length ? ',' + body.join(',') : ''}} RETURNING *`, params, options);
    },
    merge(key, value, options) {
      const params = {$id:record(key)};
      const sets = fields(value).map(([field, v], i) => { params[`$v${i}`] = v; return `${quote(field)}=$v${i}`; });
      return sets.length ? run(`UPDATE ${table} SET ${sets.join(',')} WHERE id=$id RETURNING *`, params, options)
        : run(`SELECT * FROM ${table} WHERE id=$id`, params, options);
    },
    delete(key, options) { return run(`DELETE FROM ${table} WHERE id=$id RETURNING *`, {$id:record(key)}, options); },
  });
}
