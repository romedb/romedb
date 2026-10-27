'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const { randomUUID } = require('node:crypto');
const { setTimeout: delay } = require('node:timers/promises');
const { connect, saveRequest, loadRequest } = require('../../bindings/node/client.cjs');
const { qualify } = require('./shared-client-fixture.cjs');

async function main() {
  const url = process.env.ROMEDB_FIXTURE_URL;
  const token = process.env.ROMEDB_FIXTURE_TOKEN;
  if (!url || !token) throw new Error('Set ROMEDB_FIXTURE_URL and ROMEDB_FIXTURE_TOKEN for a dedicated empty test database');
  const paceMs = Number(process.env.ROMEDB_FIXTURE_PACE_MS ?? 0);
  if (!Number.isSafeInteger(paceMs) || paceMs < 0 || paceMs > 5000) throw new Error('Invalid fixture request pacing');
  const pace = () => paceMs ? delay(paceMs) : Promise.resolve();
  const client = await connect({ url, token, timeoutMs: 120000 });
  const resumePath = process.env.ROMEDB_FIXTURE_RESUME;
  const seed = resumePath ? await loadRequest(resumePath) : undefined;
  let seedConsumed = false;
  const root = path.resolve(__dirname, '../../../../.local/tmp');
  await fs.mkdir(root, { recursive: true });
  const journals = await fs.mkdtemp(path.join(root, 'studio-url-fixture-'));
  let pending;
  const send = async (request, recovering, options) => {
      const file = path.join(journals, `${randomUUID()}.json`);
      await saveRequest(file, request);
      pending = file;
      try {
        await pace();
        const result = await (recovering ? client.recover(request, options) : client.submit(request, options));
        await fs.unlink(file);
        pending = undefined;
        return result;
      } catch (error) {
        if (error.outcome === 'rolled-back' || error.outcome === 'not-sent') {
          await fs.unlink(file);
          pending = undefined;
        }
        throw error;
      }
  };
  const recorded = {
    read: async (...args) => { await pace(); return client.read(...args); },
    prepareWrite: (...args) => client.prepareWrite(...args),
    submit: (request, options) => send(request, false, options),
    recover: (request, options) => send(request, true, options),
    writeAtomic: (statements, options) => {
      const prepared = client.prepareWrite(statements, options);
      if (seed && !seedConsumed) {
        assert.deepEqual(JSON.parse(prepared.body).statements, JSON.parse(seed.body).statements, 'saved seed must match the qualification bootstrap');
        seedConsumed = true;
        return send(seed, true, options);
      }
      return send(prepared, false, options);
    },
  };
  try {
    const metadata = (await client.read([{ sql: 'INFO FOR DB' }])).results[0].rows[0][0];
    if (seed) assert.deepEqual(metadata.tables.map(table => table.name).sort(), ['fixture_docs', 'fixture_rows'], 'resumption requires only the recovered bootstrap tables');
    else assert.deepEqual(metadata.tables, [], 'qualification requires an empty dedicated database');
    assert.deepEqual(metadata.views, [], 'qualification requires an empty dedicated database');
    const result = await qualify(recorded);
    await recorded.writeAtomic([
      { sql: 'DROP TABLE fixture_children' },
      { sql: 'DROP TABLE fixture_parents' },
      { sql: 'DROP TABLE fixture_search' },
      { sql: 'DROP TABLE fixture_docs' },
      { sql: 'DROP TABLE fixture_rows' },
    ]);
    const cleaned = (await client.read([{ sql: 'INFO FOR DB' }])).results[0].rows[0][0];
    assert.deepEqual(cleaned.tables, []);
    if (resumePath) await fs.unlink(resumePath);
    process.stdout.write(`${JSON.stringify({ ...result, cleaned: true })}\n`);
  } catch (error) {
    if (pending) process.stderr.write(`Unconfirmed request journal: ${pending}\n`);
    throw error;
  } finally {
    await client.close();
    if (!pending) await fs.rm(journals, { recursive: true, force: true });
  }
}

main().catch(error => { process.stderr.write(`${error.message}\n`); process.exitCode = 1; });
