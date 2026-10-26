'use strict';
const fs = require('node:fs/promises');
const { constants } = require('node:fs');
const path = require('node:path');
const { Buffer } = require('node:buffer');

const MAX_JOURNAL = 2_359_296;

async function saveRequest(file, request, validate) {
  validate(request);
  const text = JSON.stringify(request);
  if (Buffer.byteLength(text) > MAX_JOURNAL) throw new RangeError('request journal exceeds byte limit');
  const handle = await fs.open(file, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | (constants.O_NOFOLLOW ?? 0), 0o600);
  try {
    await handle.writeFile(text, 'utf8');
    await handle.sync();
  } catch (error) {
    await handle.close();
    await fs.unlink(file);
    throw error;
  }
  await handle.close();
  const directory = await fs.open(path.dirname(path.resolve(file)), constants.O_RDONLY | (constants.O_DIRECTORY ?? 0));
  try { await directory.sync(); }
  finally { await directory.close(); }
}

async function loadRequest(file, validate) {
  const handle = await fs.open(file, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const stat = await handle.stat();
    if (!stat.isFile() || stat.nlink !== 1 || (stat.mode & 0o077) !== 0 || (typeof process.getuid === 'function' && stat.uid !== process.getuid()) || stat.size > MAX_JOURNAL) throw new Error('request journal requires a private bounded regular file');
    const bytes = Buffer.alloc(MAX_JOURNAL + 1);
    let length = 0;
    while (length < bytes.length) {
      const result = await handle.read(bytes, length, bytes.length - length, null);
      if (!result.bytesRead) break;
      length += result.bytesRead;
    }
    if (length > MAX_JOURNAL) throw new RangeError('request journal exceeds byte limit');
    const request = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, length)));
    validate(request);
    return Object.freeze(request);
  } finally { await handle.close(); }
}

module.exports = { saveRequest, loadRequest };
