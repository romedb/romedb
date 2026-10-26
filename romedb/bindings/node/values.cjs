'use strict';
const { Buffer } = require('node:buffer');

class Float64 {
  constructor(value) {
    if (typeof value !== 'number' || !Number.isFinite(value)) throw new TypeError('Float64 requires a finite number');
    this.value = value;
    Object.freeze(this);
  }
}

class Record {
  constructor(table, key) {
    if (typeof table !== 'string' || !table || table.includes('\0') || /^(?:__romedb_|__turso_internal_|sqlite_)/i.test(table)) throw new TypeError('Record requires a nonreserved logical table name');
    if ((typeof key !== 'string' && typeof key !== 'bigint') || key === '') throw new TypeError('Record requires a nonempty string or bigint key');
    if (typeof key === 'bigint') checkInteger(key);
    this.table = table;
    this.key = key;
    Object.freeze(this);
  }
}

class Vector {
  #bytes;
  constructor(bytes) {
    if (!(bytes instanceof Uint8Array) || bytes.byteLength < 1 || bytes.byteLength > 524_289) throw new TypeError('Vector requires bounded encoded bytes');
    this.#bytes = Buffer.from(bytes);
    Object.freeze(this);
  }
  get bytes() { return Buffer.from(this.#bytes); }
  static float32(values) { return denseVector(values, 4); }
  static float64(values) { return denseVector(values, 8); }
  static sparse32Entries(dimensions, entries) {
    checkDimensions(dimensions);
    if (!Array.isArray(entries) || entries.length > dimensions) throw new TypeError('sparse entries require bounded index/value pairs');
    let previous = -1;
    const nonzero = [];
    for (const entry of entries) {
      if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError('sparse entries require index/value pairs');
      const [index, value] = entry;
      if (!Number.isInteger(index) || index <= previous || index >= dimensions) throw new RangeError('sparse indices must be increasing and within dimensions');
      checkComponent(value, 4);
      const rounded = Math.fround(value);
      if (rounded !== 0) nonzero.push([index, rounded]);
      previous = index;
    }
    const bytes = Buffer.alloc(nonzero.length * 8 + 5);
    nonzero.forEach(([index, value], i) => {
      bytes.writeFloatLE(value, i * 4);
      bytes.writeUInt32LE(index, nonzero.length * 4 + i * 4);
    });
    bytes.writeUInt32LE(dimensions, nonzero.length * 8);
    bytes[bytes.length - 1] = 9;
    return new Vector(bytes);
  }
}

function denseVector(values, width) {
  if (!Array.isArray(values) && !(values instanceof Float32Array) && !(values instanceof Float64Array)) throw new TypeError('vector components require a number array');
  checkDimensions(values.length);
  const bytes = Buffer.alloc(values.length * width + (width === 8 ? 1 : 0));
  for (let i = 0; i < values.length; i++) {
    checkComponent(values[i], width);
    if (width === 4) bytes.writeFloatLE(values[i], i * width);
    else bytes.writeDoubleLE(values[i], i * width);
  }
  if (width === 8) bytes[bytes.length - 1] = 2;
  return new Vector(bytes);
}

function checkDimensions(dimensions) {
  if (!Number.isInteger(dimensions) || dimensions < 1 || dimensions > 65_536) throw new RangeError('vector dimensions must be 1..65536');
}

function checkComponent(value, width) {
  if (typeof value !== 'number' || !Number.isFinite(value)) throw new TypeError('vector components must be finite');
  if (width === 4 && !Number.isFinite(Math.fround(value))) throw new RangeError('vector component is outside the finite float32 range');
}

function encodeValue(value, depth = 0, base64 = false) {
  checkDepth(depth);
  const tagged = (type, value) => ({ type, value });
  if (value === null) return { type: 'Null' };
  if (typeof value === 'boolean') return tagged('Boolean', value);
  if (typeof value === 'string') return tagged('String', text(value));
  if (typeof value === 'bigint') { checkInteger(value); return tagged('Integer', value.toString()); }
  if (value instanceof Float64) return encodeFloat(value.value);
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) throw new TypeError('non-finite number');
    if (Number.isInteger(value) && !Object.is(value, -0)) {
      if (!Number.isSafeInteger(value)) throw new RangeError('use bigint or Float64 for integers outside the safe Number range');
      return tagged('Integer', String(value));
    }
    return encodeFloat(value);
  }
  if (value instanceof Record) return tagged('Record', { table: text(value.table), key: encodeValue(value.key) });
  if (value instanceof Vector) return tagged('Vector', base64 ? Buffer.from(value.bytes).toString('base64') : [...value.bytes]);
  if (value instanceof Uint8Array) return tagged('Binary', base64 ? Buffer.from(value).toString('base64') : [...value]);
  if (Array.isArray(value)) {
    const entries = [];
    for (let i = 0; i < value.length; i++) {
      if (!Object.hasOwn(value, i)) throw new TypeError('array holes are not database values');
      entries.push(encodeValue(value[i], depth + 1, base64));
    }
    return tagged('Array', entries);
  }
  if (plainObject(value)) return tagged('Object', Object.fromEntries(Object.entries(value).map(([key, entry]) => [text(key), encodeValue(entry, depth + 1, base64)])));
  throw new TypeError('unsupported parameter value');
}

function byteArray(payload) {
  if (!Array.isArray(payload) || payload.length > 524_289 || payload.some(n => !Number.isInteger(n) || n < 0 || n > 255)) throw new TypeError('invalid byte array');
  return Buffer.from(payload);
}

function base64Bytes(payload) {
  if (payload.length > 1_048_576 || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(payload)) throw new TypeError('invalid base64 bytes');
  const bytes = Buffer.from(payload, 'base64');
  if (bytes.toString('base64') !== payload) throw new TypeError('invalid base64 bytes');
  return bytes;
}

function encodeFloat(value) {
  const bytes = Buffer.alloc(8);
  bytes.writeDoubleBE(value);
  return { type: 'Number', value: bytes.toString('hex') };
}

function decodeValue(value, depth = 0) {
  checkDepth(depth);
  if (!plainObject(value) || typeof value.type !== 'string') throw new TypeError('invalid tagged value');
  const keys = Object.keys(value);
  if (value.type === 'Null') {
    if (keys.length !== 1) throw new TypeError('invalid Null value');
    return null;
  }
  if (keys.length !== 2 || !Object.hasOwn(value, 'value')) throw new TypeError('invalid tagged value fields');
  const payload = value.value;
  switch (value.type) {
    case 'Boolean':
      if (typeof payload !== 'boolean') throw new TypeError('invalid Boolean value');
      return payload;
    case 'String':
      if (typeof payload !== 'string') throw new TypeError('invalid String value');
      return payload;
    case 'Integer': {
      if (typeof payload !== 'string' || !/^(?:0|-?[1-9][0-9]{0,18})$/.test(payload)) throw new TypeError('invalid canonical int64');
      const integer = BigInt(payload);
      checkInteger(integer);
      return integer;
    }
    case 'Number': {
      if (typeof payload !== 'string' || !/^[0-9a-f]{16}$/.test(payload)) throw new TypeError('invalid float64 bits');
      return new Float64(Buffer.from(payload, 'hex').readDoubleBE());
    }
    case 'Binary': case 'Vector': {
      const bytes = typeof payload === 'string' ? base64Bytes(payload) : byteArray(payload);
      return value.type === 'Vector' ? new Vector(bytes) : bytes;
    }
    case 'Record': {
      if (!plainObject(payload) || Object.keys(payload).length !== 2 || !Object.hasOwn(payload, 'table') || !Object.hasOwn(payload, 'key')) throw new TypeError('invalid Record value');
      // Historical Cloud receipts keep their original enum-key encoding.
      if (plainObject(payload.key) && Object.keys(payload.key).length === 1) {
        const kind = Object.keys(payload.key)[0];
        if (['String', 'Integer'].includes(kind)) return new Record(payload.table, decodeValue({ type: kind, value: payload.key[kind] }));
      }
      if (!['String', 'Integer'].includes(payload.key?.type)) throw new TypeError('invalid Record key');
      return new Record(payload.table, decodeValue(payload.key));
    }
    case 'Array':
      if (!Array.isArray(payload)) throw new TypeError('invalid Array value');
      return payload.map(entry => decodeValue(entry, depth + 1));
    case 'Object':
      if (!plainObject(payload)) throw new TypeError('invalid Object value');
      return Object.fromEntries(Object.entries(payload).map(([key, entry]) => [key, decodeValue(entry, depth + 1)]));
    default: throw new TypeError('unsupported value tag');
  }
}

function text(value) {
  if (!value.isWellFormed()) throw new TypeError('strings must be well-formed Unicode');
  return value;
}

function checkInteger(value) {
  if (value < -(1n << 63n) || value >= (1n << 63n)) throw new RangeError('integer outside int64');
}

function checkDepth(depth) {
  if (depth > 64) throw new RangeError('value nesting exceeds 64');
}

function plainObject(value) {
  return value !== null && typeof value === 'object' && (Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null);
}

module.exports = { Float64, Record, Vector, encodeValue, decodeValue, plainObject, text };
