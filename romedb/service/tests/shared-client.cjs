'use strict';
const fs = require('node:fs/promises');
const path = require('node:path');
const { execFile } = require('node:child_process');
const { promisify } = require('node:util');
const { connect } = require('../../bindings/node/client.cjs');
const { openEmbedded } = require('../../bindings/node/embedded-client.cjs');
const { freePort, start, stop } = require('./sdk.cjs');
const { qualify } = require('./shared-client-fixture.cjs');

async function main() {
  const root = path.resolve(__dirname, '../../../../.local/tmp');
  await fs.mkdir(root, { recursive: true });
  const directory = await fs.mkdtemp(path.join(root, 'studio-shared-client-'));
  const binary = path.resolve(process.argv[2] || path.join(__dirname, '../../../target/debug/romedb'));
  let service, client;
  try {
    client = await openEmbedded({ path: path.join(directory, 'embedded.romedb'), create: true });
    const embedded = await qualify(client);
    await client.close();
    client = null;
    service = await start(binary, path.join(directory, 'service.romedb'), await freePort(), true);
    client = await connect(service);
    const local = await qualify(client);
    await client.close();
    client = null;
    await stop(service.child);
    service = await start(binary, path.join(directory, 'url.romedb'), await freePort(), true);
    const application = await promisify(execFile)(process.execPath, [path.join(__dirname, 'url-client-fixture.cjs')], {
      env: { ...process.env, ROMEDB_FIXTURE_URL: service.url, ROMEDB_FIXTURE_TOKEN: service.token },
      timeout: 120000,
    });
    const urlApplication = JSON.parse(application.stdout);
    process.stdout.write(`${JSON.stringify({ embedded, local, urlApplication })}\n`);
  } finally {
    if (client) await client.close();
    if (service) await stop(service.child);
    await fs.rm(directory, { recursive: true, force: true });
  }
}

main().catch(error => { process.stderr.write(`${error.stack}\n`); process.exitCode = 1; });
