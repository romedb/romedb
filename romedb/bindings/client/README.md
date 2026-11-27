# RomeDB client

`@romedb/client` connects to a RomeDB database by URL, behind `romedb serve`, `romedb
serve --directory` or RomeDB Cloud. It runs in Node.js, Cloudflare Workers, browsers
and any other runtime with `fetch`.

The package is about 30 KB to download, has no dependencies and has no native addon. To
open a local database file, use [`@romedb/node`](https://www.npmjs.com/package/@romedb/node),
which depends on this package.

## Install

```sh
npm install @romedb/client
```

## A first request

```js
import { connect, Float64 } from '@romedb/client';

const rql = await connect({ url: process.env.ROMEDB_URL, token: process.env.ROMEDB_TOKEN });
const snapshot = await rql.read([{ sql: 'SELECT * FROM posts LIMIT 20' }]);
await rql.writeAtomic([
  { sql: 'UPDATE posts:p1 MERGE {score: $score}', params: { $score: new Float64(1) } },
]);
await rql.close();
```

`require('@romedb/client')` works the same way in Node.js.

## In a Cloudflare Worker

Workers need no `nodejs_compat`.

```js
import { connect } from '@romedb/client';

export default {
  async fetch(request, env) {
    const rql = await connect({ url: env.ROMEDB_URL, token: env.ROMEDB_TOKEN });
    const { results } = await rql.read([{ sql: "SELECT title FROM posts WHERE array::contains(tags, 'news')" }]);
    return Response.json({ posts: results[0].rows.length });
  },
};
```

### Requests in flight

Each client object runs up to 32 requests at once. The limit belongs to the object, not
to the database or the isolate: a 33rd request fails at once with `RDB_LIMIT` and outcome
`not-sent`. So do not start one call per row with `Promise.all`; run them in turn or in
small groups, or read the rows in one statement.

To tell this refusal apart from others, test `error.limit`, not the message. It is
`'client-requests'` when the client already runs 32 requests and `'client-bytes'` when its
requests in flight already hold too many bytes. Both pass once earlier requests end, so
you can wait and send again. A saved request that is larger than the database accepts
fails before it is sent with `'request-bytes'` or `'request-statements'`, and never
passes. An `RDB_LIMIT` from the server has no `limit`.

You can keep one client for the whole isolate. Then note that when a Worker sends its
response while some of its database calls are still running, Workers stop the I/O of those
calls, and they never finish. In 0.2.25 and earlier their places stayed taken for the life
of the isolate, so every later call failed with `RDB_LIMIT`. Since the source after 0.2.25,
a full client takes a place back once its call's `timeoutMs` has passed (20 seconds by
default), or at once when the `signal` given to the call aborts. To get places back at
once, give each incoming request an `AbortController`, pass its signal to every call and
abort it before you return:

```js
import { connect } from '@romedb/client';

let rql;

export default {
  async fetch(request, env) {
    rql ??= await connect({ url: env.ROMEDB_URL, token: env.ROMEDB_TOKEN });
    const done = new AbortController();
    try {
      const { results } = await rql.read([{ sql: 'SELECT title FROM posts LIMIT 20' }], { signal: done.signal });
      return Response.json(results[0].rows);
    } finally {
      done.abort();
    }
  },
};
```

## Value types only

`@romedb/client/values` exports `Float64`, `Record`, `Vector`, `encodeValue` and
`decodeValue`. Use it for code that never connects, such as a browser bundle.

## Using it with `@romedb/node`

`@romedb/node` depends on this package at the same version. Its `client`, `values` and
`embedded-client` entries use the classes from here, so a `Record` or `Float64` made by
one package works in the other.

That needs one installed copy of `@romedb/client`, so keep both packages at the same
version. If the versions differ, your package manager installs a second copy, and a
client refuses values made by the other copy with `unsupported parameter value`.

The `Database` and `AsyncDatabase` classes of the main `@romedb/node` entry keep their
own `Record` and `Vector`. Use the types of that entry with them.

## Saved requests

`saveRequest` and `loadRequest` keep a prepared write in a private file, so a write with
an unknown outcome can be recovered. They need Node.js file access and reject in other
runtimes. There, store the `PreparedWrite` object yourself, for example in a Durable
Object.

## URLs and tokens

A local service URL is `http://127.0.0.1:4321`. A Cloud URL names the organization
and database by stable UUIDs, and a dashboard URL that ends in `/query` also works.
Use HTTPS for every host that is not local.

`romedb://` is another name for `https://`. The client replaces the scheme and applies
the same rules, so `romedb://cloud.romedb.com/v1/organizations/{org}/databases/{db}`
and the `https://` form are the same database. Requests use `redirect: 'manual'`, and
any redirect fails with `RDB_PROTOCOL`, so the token never goes to another URL. Pass
the bearer token separately. The client refuses a token in the URL.

## Cloud URLs

For a Cloud URL, `connect()` returns without a network round trip. The database ID
comes from the URL, and the Cloud capabilities are documented. Reads start from
sequence 0 as their lower bound and update the sequence from each reply.

A write needs a current sequence. If none is known yet, `writeAtomic` first fetches
the database details, and `prepareWrite` throws `RDB_SEQUENCE_UNKNOWN` (outcome
`not-sent`) until a read, `info()` or an explicit `afterSequence` supplies one. To
fetch the details at connect, pass `deferInfo: false`. Other URLs always fetch the
details at connect and refuse `deferInfo: true`.

`connect()` on a Cloud URL also sends a warm request in the background and does not
wait for it. Cloud then starts the container of the database and opens it, so a query
a second or more later does not wait for that start. Errors of the background request
are ignored. To skip it, pass `warm: false`.

`await rql.warm()` sends the same request and returns `{ state }`. The state is
`ready` when the database is already open, `starting` when Cloud began to open it, or
`skipped` when Cloud declined because another database is busy. Call it when your
application knows that a query is coming, for example when a page or a function
starts. Local URLs refuse `warm()`.

`await rql.buildIndex(sql)` builds an index too large for one commit. On Cloud, a
`CREATE INDEX` in a query stops at about 50,000 rows, so use this for a large table. It
takes one `CREATE [UNIQUE] INDEX` or `CREATE SEARCH INDEX` statement, and waits for the
job, which takes about 4 minutes on 10 GB, and much longer for a full-text index: about
an hour for 2,000,000 documents. It returns `{ id, bytes }`, where `bytes` is the new
database size.

Reads carry on throughout. Writes get 409 only while the log folds at the start, and
again at the end while the build catches up with the writes it missed; in between they
go on. If `signal` aborts, `waitMs` runs out or the client closes, the build is canceled
and writes start again. A build
that fails throws `RDB_INDEX_BUILD` with the reason, such as duplicate values, and the
database stays as it was. Local URLs refuse `buildIndex()`.

## Files on RomeDB Cloud

A Cloud database has managed file storage: your application uploads files, RomeDB keeps
them in private storage, and you keep the file id in your own records. The bytes go from
the browser straight to storage in parts, so there is no file size limit below 4.995 TiB
other than your organization's storage allowance, and an upload that stops can continue.

RomeDB does not know your users. Your backend decides who may upload, and gives the page
an upload session: a permission for one file, with a token that works for that upload
only. The page never gets a database token or an organization key.

**The backend** needs a database token with the scope `files:write` (and `files:read`,
`files:delete` for the calls below), or an organization key with `manage`:

```js
import { connect } from '@romedb/client';

const db = await connect({ url: env.ROMEDB_URL, token: env.ROMEDB_FILES_TOKEN });

// POST /api/uploads/authorize: the page sends { id, name, size, type, context }.
async function authorizeUpload(request) {
  const user = await requireUser(request);              // your authentication
  const intent = await request.json();                  // from the browser: check it
  await requireEditableArticle(user, intent.context.articleId);
  if (intent.size > 25 * 1024 * 1024) return new Response('File too large', { status: 413 });
  const session = await db.files.createUploadSession({
    id: intent.id, name: intent.name, size: intent.size, mediaType: intent.type,
    allowedMediaTypes: ['image/jpeg', 'image/png', 'application/pdf'],
    subject: user.id,
  });
  return Response.json(session, { headers: { 'cache-control': 'no-store' } });
}
```

**The page** imports only `@romedb/client/uploads`, which holds no code that takes a
database token:

```js
import { createUploader } from '@romedb/client/uploads';

const uploader = createUploader({
  authorize: async intent => {
    const response = await fetch('/api/uploads/authorize', {
      method: 'POST', credentials: 'same-origin',
      headers: { 'content-type': 'application/json', 'x-csrf-token': csrfToken() },
      body: JSON.stringify(intent),
    });
    if (!response.ok) throw new Error('Upload not allowed');
    return response.json();
  },
});

const controller = new AbortController();
const file = await uploader.upload(input.files[0], {
  context: { articleId },
  signal: controller.signal,
  onProgress: ({ uploadedBytes, totalBytes }) => showProgress(uploadedBytes / totalBytes),
});
await attachToArticle(articleId, file.id);              // your backend checks again
```

`upload()` resolves when Cloud has published the file. It sends 4 parts at a time, sends
a failed part again, and compares a checksum of the file with the one storage computed
(for files up to 1 GiB, or always with `checksum: true`). Progress moves in steps of one
part, 8 MiB for most files.

To continue an upload after a reload or a failure, call `upload()` with the same file
and the same `id`: the backend's `createUploadSession` returns the same session for the
same `id`, and only the missing parts are sent. An `UploadError` says in `recoverable`
whether that can work. `controller.abort()` stops the transfer in the page only; the
session stays open in Cloud until `uploader.abort(session)` or its expiry (24 hours by
default), and its reserved space counts against your allowance until then.

**Afterwards, on the backend:**

```js
const file = await db.files.get(fileId);                // size, mediaType, checksum, metadata...
const page = await db.files.list({ limit: 50, order: 'desc' });
await db.files.update(fileId, { description: 'Signed copy', tags: ['contract'] });
const { url } = await db.files.createDownloadUrl(fileId, { expiresInSeconds: 300 });
const deletion = await db.files.delete(fileId);         // { state: 'done' | 'pending' }
const stored = await db.files.upload(await fs.openAsBlob('report.pdf'), { name: 'report.pdf' });
```

A download URL works for anyone who has it until it expires, and it cannot be taken back.
Check your user's permission before you create one, and keep its life short. Files
download as attachments; `disposition: 'inline'` is allowed only for images, PDF, plain
text, audio and video. `delete` removes the record at once; `state` is `'pending'` while
storage still removes the bytes, and `db.files.deletionStatus(fileId)` reports it later.

A file id is not a permission. Anyone who holds your backend's token can read or delete
every file of the database, so the token stays on the server.

## Local directories

An organization URL on a loopback host, such as a database of `romedb serve
--directory` (`http://127.0.0.1:4321/v1/organizations/{org}/databases/{db}`), always
fetches the details at connect. A reply with `serviceVersion: 1` is a local service.
The client then negotiates its capabilities and the service's request limits, and uses
the local service request format, with no warm request. Any other reply is Cloud running locally, which keeps
the Cloud request format. So an application can use the same organization URLs for a
local directory and for Cloud.

## Values and results

Results use positional rows and keep duplicate columns. Integer results are `bigint`.
Number results are `Float64` instances with a `.value` number. They keep their exact
float type when you bind them again, integral floats and negative zero included.

Encoded vectors and the dense float32, float64 and sparse-entry factories
work without native code. `null` is a present field. `undefined` and array holes are
refused, so no data disappears without an error.

## Errors and timeouts

Requests and responses have finite byte and row limits. `rql.limits` gives the ones in
force: `maxRequestBytes`, `maxStatements`, `maxRows`, `maxResultBytes` and
`maxResponseBytes`, and on RomeDB Cloud also `maxCommitBytes` and `maxCommitRows`: the log
one write may add and the rows it may change (1 MiB and 100,000, or 256 MiB and
5,000,000 where Cloud runs big commits). The client cannot check those two before it
sends a write; a write past them fails with `RDB_LIMIT` and changes nothing. A local service reports them in its details (8 MiB requests, 256
statements, 10,000 rows per result, 8 MiB of results and 16 MiB responses since the
source after 0.2.22), and the client checks every request and reply against them, so a
request over a limit throws before it is sent. Every read and write sends `maxRows` and
`maxResultBytes` in the `x-romedb-result-limits` header, and the server keeps its results
within them. A Cloud connection that has not read its details uses Cloud's documented
values: 1 MiB requests, 256 statements, 10,000 rows, 8 MiB of results and 9 MiB
responses. Cloud keeps write results at 256 KiB, because it stores them for retries; a
larger one fails with `RDB_LIMIT`. A server that reports none keeps the earlier values: 64 KiB requests, 32
statements, 1,000 rows, 256 KiB of results and 512 KiB responses. Size batches from
`rql.limits` instead of copying the numbers. Each call times out after 20
seconds by default and accepts `signal` and `timeoutMs` (up to 600,000, 10 minutes). A
big commit on Cloud waits for the database's other writes and can run for minutes, so
pass a longer `timeoutMs` for a write that changes more than about 1 MiB. A write that
times out on the client may still commit: make it with `prepareWrite` and send it with
`submit`, and after a timeout pass the same `PreparedWrite` to `recover`, so it commits
once. An HTTP error reply with a
`Retry-After` header sets `ClientError.retryAfterMs`. A transport or response failure
without a database reply keeps the underlying error as `ClientError.cause`, for
example a `fetch failed` TypeError with `ECONNREFUSED`.

`rql.sequenceKnown` is `true` once a reply has given the current sequence. `rql.cloud`
tells a Cloud database from a local service. A write cancelled after it was sent stays
unknown until a receipt confirms it. Cloud capabilities are marked as documented,
because its API does not negotiate the local capability object. See the
[service contract](https://github.com/romedb/romedb/blob/main/romedb/docs/service-protocol.md) for limits, receipt rules and
compatibility gaps.

## More

The [Node.js guide](https://github.com/romedb/romedb/blob/main/romedb/bindings/node/README.md#url-client)
has the full client reference, including saved requests, query versions and file
exports. The client methods are the same in both packages.
