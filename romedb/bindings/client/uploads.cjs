'use strict';
// Uploads to RomeDB Cloud managed files, for browsers, Workers and Node.js.
//
// This module never sees a database token or an organization key. It works with an upload
// session: a short-lived permission for one file, which a backend opens with
// `db.files.createUploadSession()` and hands to the page. With the session the module asks
// Cloud for one URL per part, sends each part straight to storage, and asks Cloud to
// publish the file. Only Web APIs are used.

const MAX_PARTS = 10000;
const CHECKSUM_DEFAULT_BYTES = 1024 * 1024 * 1024;
const CHECKSUM_CHUNK = 4 * 1024 * 1024;
const PART_ATTEMPTS = 5, CALL_ATTEMPTS = 4, COMPLETE_ROUNDS = 3;
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

/**
 * An upload that did not finish. `recoverable` tells whether calling upload() again with
 * the same `id` can continue it; `session` is the session it used, when it had one.
 */
class UploadError extends Error {
  constructor(code, message, { recoverable = false, status, session, cause } = {}) {
    super(message, cause === undefined ? undefined : { cause });
    this.name = 'UploadError';
    this.code = code;
    this.recoverable = recoverable;
    if (status !== undefined) this.status = status;
    if (session !== undefined) Object.defineProperty(this, 'session', { value: session, enumerable: false });
  }
}

function plainObject(value) { return value !== null && typeof value === 'object' && !Array.isArray(value); }
function asBlob(source) {
  if (typeof Blob !== 'undefined' && source instanceof Blob) return source;
  if (source instanceof ArrayBuffer || ArrayBuffer.isView(source)) return new Blob([source]);
  throw new TypeError('upload takes a File, a Blob, an ArrayBuffer or a typed array');
}
function checkSession(session) {
  if (!plainObject(session) || !uuid.test(session.id ?? '') || typeof session.url !== 'string' || !/^https?:\/\//.test(session.url) ||
      typeof session.token !== 'string' || !/^rdbu_[A-Za-z0-9_-]{20,200}$/.test(session.token) ||
      !Number.isSafeInteger(session.size) || session.size < 1 || !Number.isSafeInteger(session.partSize) || session.partSize < 1 ||
      !Number.isInteger(session.parts) || session.parts < 1 || session.parts > MAX_PARTS || session.parts !== Math.ceil(session.size / session.partSize))
    throw new UploadError('UPLOAD_PROTOCOL', 'The upload session is not one that RomeDB Cloud issued');
  return session;
}
function pause(ms, signal) {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) return reject(cancelled());
    const timer = setTimeout(() => { signal?.removeEventListener('abort', stop); resolve(); }, ms);
    const stop = () => { clearTimeout(timer); reject(cancelled()); };
    signal?.addEventListener('abort', stop, { once: true });
  });
}
function cancelled(session) { return new UploadError('UPLOAD_CANCELLED', 'Upload cancelled here; the session stays open until it is aborted or expires', { recoverable: true, session }); }
const backoff = attempt => Math.min(8000, 400 * 2 ** attempt) * (0.5 + Math.random() / 2);

// ---- CRC-64/NVME, the checksum R2 reports for a whole object ----

let crcTables;
function tables() {
  if (crcTables) return crcTables;
  const high = new Uint32Array(256), low = new Uint32Array(256);
  for (let n = 0n; n < 256n; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1n ? (c >> 1n) ^ 0x9a6c9329ac4bc9b5n : c >> 1n;
    high[Number(n)] = Number(c >> 32n); low[Number(n)] = Number(c & 0xffffffffn);
  }
  return (crcTables = [high, low]);
}
/** A running CRC-64/NVME: update() with bytes in order, then base64() for the value R2 reports. */
function crc64nvme() {
  const [highs, lows] = tables();
  let high = 0xffffffff, low = 0xffffffff;
  return {
    update(bytes) {
      for (let at = 0; at < bytes.length; at++) {
        const index = (low ^ bytes[at]) & 0xff;
        low = (((low >>> 8) | (high << 24)) ^ lows[index]) >>> 0;
        high = ((high >>> 8) ^ highs[index]) >>> 0;
      }
      return this;
    },
    base64() {
      const h = (high ^ 0xffffffff) >>> 0, l = (low ^ 0xffffffff) >>> 0;
      return btoa(String.fromCharCode(h >>> 24, (h >>> 16) & 255, (h >>> 8) & 255, h & 255, l >>> 24, (l >>> 16) & 255, (l >>> 8) & 255, l & 255));
    },
  };
}
async function checksumOf(blob, signal) {
  const crc = crc64nvme();
  for (let at = 0; at < blob.size; at += CHECKSUM_CHUNK) {
    if (signal?.aborted) throw cancelled();
    crc.update(new Uint8Array(await blob.slice(at, at + CHECKSUM_CHUNK).arrayBuffer()));
  }
  return crc.base64();
}

// ---- calls to Cloud with the session token ----

function sessionCalls(session, fetch, signal) {
  async function call(method, path, body) {
    for (let attempt = 0; ; attempt++) {
      if (signal?.aborted) throw cancelled(session);
      let response;
      try {
        response = await fetch(session.url + path, {
          method, redirect: 'manual', signal,
          headers: { authorization: `Bearer ${session.token}`, accept: 'application/json', ...(body === undefined ? {} : { 'content-type': 'application/json' }) },
          ...(body === undefined ? {} : { body: JSON.stringify(body) }),
        });
      } catch (cause) {
        if (signal?.aborted) throw cancelled(session);
        if (attempt + 1 < CALL_ATTEMPTS) { await pause(backoff(attempt), signal); continue; }
        throw new UploadError('UPLOAD_TRANSPORT', 'RomeDB Cloud could not be reached', { recoverable: true, session, cause });
      }
      const status = response.status;
      if (status >= 500 || status === 429) {
        await response.body?.cancel().catch(() => {});
        if (attempt + 1 < CALL_ATTEMPTS) { await pause(backoff(attempt), signal); continue; }
        throw new UploadError(status === 429 ? 'UPLOAD_LIMIT' : 'UPLOAD_TRANSPORT', 'RomeDB Cloud did not answer the upload request', { recoverable: true, status, session });
      }
      let reply;
      try { reply = await response.json(); } catch { reply = undefined; }
      if (response.ok && plainObject(reply)) return reply;
      const message = typeof reply?.error === 'string' ? reply.error.slice(0, 500) : 'Upload request was refused';
      if (status === 401 || status === 403) throw new UploadError('UPLOAD_AUTHORIZATION', message, { status, session });
      if (status === 404) throw new UploadError('UPLOAD_SESSION', message, { status, session });
      if (status === 409) throw new UploadError('UPLOAD_CONFLICT', message, { recoverable: true, status, session });
      throw new UploadError('UPLOAD_PROTOCOL', message, { status, session });
    }
  }
  return {
    status: () => call('GET', ''),
    parts: numbers => call('POST', '/parts', { parts: numbers }),
    complete: expected => call('POST', '/complete', expected),
    abort: () => call('POST', '/abort'),
  };
}
function closedSession(status, session) {
  if (status.state === 'rejected') return new UploadError('UPLOAD_REJECTED', status.reason ?? 'The upload was rejected', { session });
  return new UploadError('UPLOAD_SESSION', `The upload session is ${status.state}${status.reason ? `: ${status.reason}` : ''}`, { session });
}

// ---- the transfer ----

/**
 * Sends `source` through an open session and returns the published file. Parts that
 * storage already holds are not sent again, so the same call continues an upload that
 * stopped. Options: fetch, signal, onProgress, concurrency (1 to 16, default 4), checksum.
 */
async function uploadToSession(session, source, options = {}) {
  checkSession(session);
  const blob = asBlob(source);
  if (blob.size !== session.size) throw new UploadError('UPLOAD_SOURCE', 'This is not the file the session was opened for: its size differs', { session });
  const fetch = options.fetch ?? globalThis.fetch.bind(globalThis), signal = options.signal;
  if (signal !== undefined && !(signal instanceof AbortSignal)) throw new TypeError('signal must be an AbortSignal');
  const concurrency = options.concurrency ?? 4;
  if (!Number.isInteger(concurrency) || concurrency < 1 || concurrency > 16) throw new TypeError('concurrency must be 1..16');
  const calls = sessionCalls(session, fetch, signal);
  const partBytes = number => number === session.parts ? session.size - (session.parts - 1) * session.partSize : session.partSize;
  const report = sent => {
    if (typeof options.onProgress !== 'function') return;
    let uploadedBytes = 0;
    for (const number of sent) uploadedBytes += partBytes(number);
    options.onProgress({ uploadedBytes, totalBytes: session.size, uploadedParts: sent.size, totalParts: session.parts });
  };
  // The checksum reads the file once from start to end while the parts are sent.
  const checksum = (options.checksum ?? blob.size <= CHECKSUM_DEFAULT_BYTES) ? checksumOf(blob, signal) : null;
  checksum?.catch(() => {});

  let status = await calls.status();
  for (let round = 0; round < COMPLETE_ROUNDS; round++) {
    if (status.state === 'ready' && plainObject(status.file)) return status.file;
    if (status.state !== 'uploading' && status.state !== 'verifying') throw closedSession(status, session);
    const sent = new Set(Array.isArray(status.uploadedParts) ? status.uploadedParts.filter(number => Number.isInteger(number) && number >= 1 && number <= session.parts) : []);
    report(sent);
    const pending = [];
    for (let number = 1; number <= session.parts; number++) if (!sent.has(number)) pending.push(number);
    await sendParts(pending, sent);
    try {
      const done = await calls.complete(checksum ? { crc64nvme: await checksum } : {});
      if (!plainObject(done.file)) throw new UploadError('UPLOAD_PROTOCOL', 'Completion did not return a file', { session });
      return done.file;
    } catch (error) {
      if (!(error instanceof UploadError) || error.code !== 'UPLOAD_CONFLICT') throw error;
      // Storage lost a part, or the upload ended elsewhere: the status tells which.
      status = await calls.status();
      if (status.state === 'uploading' || status.state === 'verifying') await pause(backoff(round), signal);
    }
  }
  if (status.state === 'ready' && plainObject(status.file)) return status.file;
  if (status.state !== 'uploading' && status.state !== 'verifying') throw closedSession(status, session);
  throw new UploadError('UPLOAD_TRANSPORT', 'Storage kept losing parts of the upload', { recoverable: true, session });

  async function sendParts(pending, sent) {
    // URLs live for minutes, so they are asked for a few parts ahead and not for the whole file.
    const wave = Math.min(100, concurrency * 4);
    for (let at = 0; at < pending.length; at += wave) {
      const numbers = pending.slice(at, at + wave);
      const urls = new Map((await calls.parts(numbers)).parts?.map(part => [part.partNumber, part.url]));
      let next = 0, failure;
      const worker = async () => {
        while (next < numbers.length && !failure) {
          const number = numbers[next++];
          try { await sendPart(number, urls.get(number)); sent.add(number); report(sent); }
          catch (error) { failure ??= error; }
        }
      };
      await Promise.all(Array.from({ length: Math.min(concurrency, numbers.length) }, worker));
      if (failure) throw failure;
    }
  }
  async function sendPart(number, url) {
    const start = (number - 1) * session.partSize, body = blob.slice(start, start + partBytes(number));
    for (let attempt = 0; ; attempt++) {
      if (signal?.aborted) throw cancelled(session);
      if (typeof url !== 'string') url = (await calls.parts([number])).parts?.[0]?.url;
      if (typeof url !== 'string') throw new UploadError('UPLOAD_PROTOCOL', 'RomeDB Cloud did not return a part URL', { session });
      let response;
      try { response = await fetch(url, { method: 'PUT', body, signal }); }
      catch (cause) {
        if (signal?.aborted) throw cancelled(session);
        if (attempt + 1 < PART_ATTEMPTS) { await pause(backoff(attempt), signal); continue; }
        throw new UploadError('UPLOAD_TRANSPORT', `Part ${number} could not be sent`, { recoverable: true, session, cause });
      }
      await response.body?.cancel().catch(() => {});
      if (response.ok) return;
      const last = attempt + 1 >= PART_ATTEMPTS;
      // 403: the URL expired or the upload ended. A fresh URL tells which: asking for one fails when the upload is closed.
      if (response.status === 403 && !last) { url = undefined; continue; }
      if ((response.status >= 500 || response.status === 429 || response.status === 408) && !last) { await pause(backoff(attempt), signal); continue; }
      throw new UploadError(response.status >= 500 || response.status === 429 ? 'UPLOAD_TRANSPORT' : 'UPLOAD_PROTOCOL', `Storage refused part ${number} (HTTP ${response.status})`,
        { recoverable: response.status >= 500 || response.status === 429, status: response.status, session });
    }
  }
}

/**
 * The uploader of a page. `authorize(intent)` asks the page's own backend for an upload
 * session: the backend checks its user and calls `db.files.createUploadSession()`. The
 * intent is { id, name, size, type, context }; pass `id` on to createUploadSession so that
 * a second upload() with the same id continues the first.
 */
function createUploader(options) {
  if (!plainObject(options) || typeof options.authorize !== 'function') throw new TypeError('createUploader requires an authorize function');
  if (options.fetch !== undefined && typeof options.fetch !== 'function') throw new TypeError('fetch must be a function');
  const fetch = options.fetch ?? globalThis.fetch.bind(globalThis);
  return {
    async upload(source, settings = {}) {
      if (!plainObject(settings)) throw new TypeError('upload options require an object');
      const blob = asBlob(source), id = settings.id ?? crypto.randomUUID();
      if (!uuid.test(id)) throw new TypeError('id must be a UUID');
      if (blob.size < 1) throw new UploadError('UPLOAD_SOURCE', 'An empty file cannot be uploaded');
      const name = settings.name ?? source.name;
      if (typeof name !== 'string' || !name) throw new TypeError('name is required when the source is not a File');
      const intent = { id, name, size: blob.size, type: settings.type ?? (blob.type || 'application/octet-stream'), ...(settings.context === undefined ? {} : { context: settings.context }) };
      let session;
      try { session = await options.authorize(intent); }
      catch (cause) { throw new UploadError('UPLOAD_AUTHORIZATION', 'The backend did not authorize the upload', { recoverable: true, cause }); }
      checkSession(session);
      if (typeof settings.onSession === 'function') settings.onSession(session);
      return uploadToSession(session, blob, { fetch, concurrency: options.concurrency, ...settings });
    },
    /** What Cloud knows about the upload: its state and the parts storage holds. */
    status(session, { signal } = {}) { return sessionCalls(checkSession(session), fetch, signal).status(); },
    /** Ends the upload in Cloud and frees its reserved space. Stopping locally does not do this. */
    abort(session, { signal } = {}) { return sessionCalls(checkSession(session), fetch, signal).abort(); },
  };
}

module.exports = { createUploader, uploadToSession, UploadError, crc64nvme };
