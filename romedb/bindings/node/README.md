# RomeDB for Node.js

RomeDB is an embedded database. You query it with RomeQL, a document query
language that also accepts SQL. This package has two parts:

- A native addon that runs RomeDB inside your Node.js process. It is built for
  Linux x64 and needs Node.js 22 or later. Node 22 and 24 are the tested versions.
- A client that connects to `romedb serve` or RomeDB Cloud by URL. It uses only Web
  APIs, so it also runs in Cloudflare Workers and in browsers.

Other operating systems and architectures are outside this release. The addon does
not expose the raw Turso connection. One process owns a database file at a time.
The [deployment contract](https://github.com/romedb/romedb/blob/main/romedb/docs/deployment.md) describes this rule.

## Install

```sh
npm install @romedb/node
```

Each release publishes its Node package asset to npm unchanged. The first npm
version, 0.2.12, differs from its asset only in `package.json` and this README. A
project can also depend on the asset of a GitHub release by URL:

```sh
pnpm add https://github.com/romedb/romedb/releases/download/romedb-v0.2.16/romedb-node-0.2.16.tgz
```

From 0.2.15, `@romedb/node` depends on `@romedb/client` at the same version, so that
asset also needs npm or the `romedb-client` asset of the same release.

## A first database

```js
const { Database, Record } = require('@romedb/node');
const rql = new Database('app.romedb');
try {
  // The first document write creates the posts collection.
  rql.execute('INSERT INTO posts DOCUMENT $doc', {
    $doc: { id: new Record('posts', 'p1'), title: 'Hello', visits: 1n },
  });
  rql.execute('UPDATE posts:p1 MERGE {visits: 2}');
  const [post] = rql.exactlyOne('SELECT * FROM posts:p1');
  console.log(post.title, post.visits); // Hello 2n
} finally {
  rql.close();
}
```

`posts:p1` is a record ID: the collection `posts` and the key `p1`. In
JavaScript, `new Record('posts', 'p1')` is the same ID. `$doc` is a parameter, so
the values never become part of the statement text. Integer results are always
`bigint`.

Use this package in your Node.js backend. A browser calls your backend API, and
RomeDB runs and stores the database inside the server process. For request
handlers, use [AsyncDatabase](#asynchronous-database) or
[openEmbedded](#embedded-client), which keep database work off the main JavaScript
thread. The synchronous `Database` suits scripts and other blocking work.

`Database` and `AsyncDatabase` open MVCC files and WAL files, and they create new
files in WAL mode. `openEmbedded`, `romedb serve` and RomeDB Cloud use only MVCC
files. To use a WAL file with them, convert it with `romedb migrate-to-mvcc`, or
create the file with `openEmbedded` from the start.

## RomeQL at a glance

| Statement | What it does |
| --- | --- |
| `INSERT INTO posts {id: posts:hello, title: 'Hello', tags: ['news']}` | Adds a document. If the collection does not exist, the write creates it. |
| `UPSERT people:alice {name: 'Alice', role: 'editor'}` | Creates the record, or merges these fields into it. |
| `SELECT * FROM posts:hello` | Reads one record by its ID. |
| `UPDATE posts:hello MERGE {visits: 2}` | Changes the given fields and keeps the others. |
| `SELECT id, title FROM posts WHERE array::contains(tags, 'news')` | Finds documents by a value in an array. |
| `INSERT INTO posts {id: posts:second, title: 'Second', author: people:alice}` | Stores a link to another record. |
| `SELECT * FROM posts WHERE id = posts:second FETCH author` | Returns the post with the linked `people:alice` document in place of the link. |
| `DELETE posts:hello` | Deletes one record. `DELETE FROM posts:hello` is the same statement. |
| `DEFINE FIELD title ON posts TYPE string` | Adds validation. After it, a write with a number in `title` fails with `RDB_VALIDATION`. |
| `CREATE UNIQUE INDEX posts_title ON posts(title)` | Adds a unique index on a field. |
| `CREATE TABLE visits(at INTEGER, path TEXT)` | Makes an ordinary SQL table. SQL statements work on it, and collections can join it. |

The [language guide](https://github.com/romedb/romedb/blob/main/romedb/docs/v2.2-language.md) describes the full language. The
[document SDK guide](https://github.com/romedb/romedb/blob/main/romedb/docs/node-sdk.md) describes the typed `collection(name)`
helpers.

## Choose an API

| API | Entry | Use it for |
| --- | --- | --- |
| `Database` | `@romedb/node` | Scripts and tools. Each call blocks the JavaScript thread. |
| `AsyncDatabase` | `@romedb/node` | Servers. Queries run on a worker thread, in the order you send them. |
| `openEmbedded` | `@romedb/node/embedded-client` | A local MVCC file with the same request interface as the URL client: atomic batches, sequences and write recovery. |
| `connect` | `@romedb/node/client` | A database behind `romedb serve` or RomeDB Cloud. Runs in Node.js, Workers and browsers. |

## Database

### Results

`execute(sql, parameters)` returns an object with these fields:

- `columns`: the column names.
- `rows`: positional rows of typed values.
- `affected`: the count of changed rows, as a `bigint`.
- `transaction`: the transaction state before and after the statement.

`all`, `first` and `exactlyOne` return only rows. `first` returns `undefined` when
there is no row. `exactlyOne` throws a `RangeError` with the code `RDB_CARDINALITY`
unless the result has exactly one row. That error has the `transaction` field of
the statement. The statement already ran, so the helper does not undo writes or
roll back an outer transaction.

### The connection

`Database` methods are synchronous and block the calling JavaScript thread. Each
instance owns one frontend connection and the lifetime of its database. You can
call `close()` more than once. Calls after `close()` fail. Closing during an active
transaction uses the normal connection-drop behavior of the engine. Do not share
native handles across workers. Use `AsyncDatabase` to give a dedicated worker its
own connection.

### Values

| JavaScript | RomeDB |
| --- | --- |
| `bigint`, or a `Number` that is a safe integer | int64. Integer results are always `bigint`. |
| A `Number` with a fraction, and `-0` | binary64, with the exact bits kept |
| A `Number` that is an unsafe integer | Refused. Use a `bigint`. |
| `boolean`, `null`, `string` | The same type |
| Array, plain object | Array, object |
| `Buffer`, `Uint8Array` | Binary |
| `Record(table, key)` | A record ID. The key is a string or a `bigint`. `String(record)` gives `table:key`, and `record.equals(other)` compares the table and the key. |
| `Vector` | A vector of validated native-format bytes. See [typed vectors](#typed-vectors). |

RomeDB never makes a reference from a string. Unsupported JavaScript types,
non-finite numbers, int64 values out of range and too much nesting fail. The bridge
reads only the own entries of an object, so a field named like a prototype property
stays data. Values travel in the transfer-v1 tagged encoding.

Standalone projections such as `SELECT $object AS value` keep the types of
composite, record, vector and boolean parameters. Typed helper expressions and
DOCUMENT writes keep them too. Type propagation through other source queries is
not finished. Prepared statements and streamed results are not available.

### Errors

Engine and frontend failures throw an `Error` with a `code` and a `transaction`
field. A call on a closed `Database` or `AsyncDatabase`, or on a worker that is
closing, fails with `RDB_CLOSED`. That error has no `transaction` field, because no
statement ran. Constructor errors and JavaScript argument errors are ordinary
errors without these fields. The error contract across clients is not final.

| Code | Meaning |
| --- | --- |
| `RDB_BUSY` | Native lock contention. |
| `RDB_BUSY_SNAPSHOT` | A stale read transaction cannot become a writer. The work needs a fresh transaction. |
| `RDB_CONSTRAINT` | A native constraint, foreign-key or trigger-raise error, with the engine message. |
| `RDB_VALIDATION` | Frontend validation of a document failed. |
| `RDB_CLOSED` | The database or its worker is closed. |

Before you recover from an error, read its `transaction` field. The client does not
retry by itself. `RDB_CONSTRAINT` also covers some runtime validation failures, and
it does not tell the rollback scope: ordinary SQL `OR FAIL` can keep earlier rows.
Apply the conflict policy of the statement when you recover. A test with two
`Database` instances on one file covers both busy codes. Contention and recovery
across processes need more qualification.

`isRomeDBError(error)` narrows a caught `unknown` value to the exported
`RomeDBError` interface. It accepts an `Error` with an `RDB_*` code. It also
validates the transaction and migration details when they are present. The
`transaction` field is optional, because closed-handle, worker and queue errors can
happen before execution. Constructor and argument errors can fail this test.

### Migrations

`migrate(migrations)` takes the complete ordered history as
`{ version: bigint, name: string, sql: string }` entries. It returns
`{ alreadyApplied: number, applied: bigint[], transaction }`. The rules of the Rust
runner apply unchanged. It needs autocommit and compares applied scripts with their
exact source. It has limits, and it runs all pending scripts atomically. The Node
client does not read migration folders, so your code supplies the scripts.

| Migration failure | Error and meaning |
| --- | --- |
| Script splitting or tokenization | `RDB_SYNTAX`, with the migration version and the byte offset in that script. The plan did not start. |
| Edited or incomplete applied history | `RDB_VALIDATION`. The message tells whether the version sequence, the name or the exact SQL differs. Restore the original applied scripts before you retry. |
| Size limit of the input or the stored history | `RDB_LIMIT`. The message names the limit. |
| Incompatible history schema or invalid stored types | `RDB_STORAGE`. The runner does not repair the history. |
| A pending statement fails | Usually `RDB_MIGRATION`. `error.migration.version` and `.offset` are `bigint` values, and `.cause.code` and `.cause.message` give the underlying failure. The offset counts UTF-8 bytes in that script. The runner rolls back all pending scripts and their history together. |
| Cooperative cancellation | `RDB_CANCELLED`. The message of a pending statement keeps its version and offset. Completion can win the race with the abort. |
| Rollback failure | `RDB_ROLLBACK`. Cleanup did not reach the normal rollback outcome. |

Migration errors keep `code`, `message` and the transaction observations of the
runner in both clients. The version and the UTF-8 byte offset are in the message.
There are no separate JavaScript `version` or `offset` properties on the error.

The optional `migration` property has the exported `MigrationFailure` shape. Wrapped
statement failures have it, interrupted statements included. Plan parsing, history
comparison and cancellation before execution do not always provide it.

Before you retry, read `error.transaction` and the actual result. A worker
transport failure (`RDB_WORKER`) has no transaction observation, so it does not tell
whether accepted migration work committed. JavaScript argument validation can fail
before the runner starts. Keep applied scripts unchanged, comments and whitespace
included. After a confirmed failed run, you can correct a pending script that is not
recorded yet.

### Document import and export

`exportDocuments(table, format = 'json')` returns the versioned typed transfer
string. `importDocuments(table, input, format = 'json')` returns
`{ imported: number, transaction }`. The format is `json` or `ndjson`. Imports go
into an existing collection and keep its validation, indexes and rollback behavior.

These methods hold the whole string in memory and use the transfer limits of the
frontend. They do not back up the schema, stream data or read or write files. To
copy a whole database into a file, use [`exportFile`](#copying-a-database-into-a-file).
Failures use the same code and transaction fields as `execute`. Closed-handle calls
fail with `RDB_CLOSED` and no transaction field.

## Asynchronous database

```js
const { AsyncDatabase } = require('@romedb/node');
const rql = await AsyncDatabase.open('app.romedb');
try {
  await rql.execute("UPSERT people:alice {name: 'Alice', active: true}");
  console.log(await rql.all('SELECT name FROM people WHERE active = true')); // [ [ 'Alice' ] ]
} finally {
  await rql.close();
}
```

`AsyncDatabase` has the methods of `Database` and returns Promises with the same
values and results. It also has `exportFile(destination, options)`, which copies an
MVCC database into a new file while queries continue and resolves to `{ bytes }`.

### How it runs

`AsyncDatabase.open()` waits for the native database to open and rejects if it
fails. One dedicated [Node worker](https://nodejs.org/api/worker_threads.html) owns
the native connection. Requests run one at a time, in the order you send them.
Each `AsyncDatabase` instance has its own worker and connection. The database work
runs off the event loop of the caller. Argument encoding, message copies and result
decoding still use time and memory in the caller.

### Queue and closing

The queue accepts 256 outstanding operations and 128 MiB of encoded argument
strings, the active operation included. A request over these limits rejects with
`RDB_LIMIT` before it is sent. These limits do not bound total memory or query time.

`close()` stops new requests, runs the requests already accepted, closes the native
connection and waits for the worker to exit. Repeated `close()` calls share the same
Promise. Closing during an active transaction rolls it back, through the native
drop behavior that the reopen tests cover. You must await `close()` to release the
worker. There is no automatic idle shutdown and no force-termination API.

### Transactions and errors

A query error rejects only its own Promise. Later queued requests still run. An
explicit transaction belongs to the whole connection, not to one caller or one
Promise chain. Your application must decide who owns a transaction, and it must
await each step before it sends dependent work. The API has no transaction callback
that isolates callers.

An unexpected worker error or exit rejects every outstanding request. Recovery
after a worker crash, accounting of native memory, more cancellation tests and
long-running lifecycle stress are still release gates.

### Interrupting a statement

`interrupt()` asks the engine to stop the active statement. It runs at once and does
not wait in the worker queue. It returns `false` after native close, and `true` while
the connection is live. `true` does not confirm that the statement stopped.

A cancelled `execute` rejects with `RDB_CANCELLED` and the observed transaction
state, and the worker can run later operations. The interruption applies to the
whole connection: it does not target one Promise and does not remove queued
requests. Stop repeated interrupt requests before you use the connection again.
Parsing, work outside the engine and a running bundled function do not stop by
interruption, and there is no hard deadline.

The addon keeps a process-local registry of weak interrupt handles and removes an
entry on native close or drop. No raw engine pointer crosses into JavaScript.

### Worker failures

A fatal worker transport error uses `RDB_WORKER`, keeps the first cause and rejects
every outstanding request. It has no transaction observation: a missing response
does not tell whether an accepted write committed. Later requests reject with the
same failure. Do not retry a write with an unknown outcome automatically.

After such a failure, `close()` waits for the worker to exit. If the failure happens
during a `close()` that is already running, that `close()` rejects after cleanup
ends. When a message cannot be deserialized, the client asks for a graceful native
close through the remaining channel. If that send fails too, cleanup terminates the
worker. This fallback is separate from query interruption, and it does not prove
database recovery or the outcome of a write.

Isolated fault-injection tests cover the state changes of the worker transport.
Native crashes, interrupted commit or recovery and the resources of a forced
termination need separate release qualification.

### Script batches

`executeBatch(script)` runs a semicolon-separated script and returns one entry per
statement. `AsyncDatabase` sends the whole batch as one worker operation. Each entry
is one of these:

- `{ offset, transaction, result }`, where `result` has columns, typed positional
  rows and a `bigint` affected count.
- `{ offset, transaction, error: { code, message } }`.

Offsets are UTF-8 byte positions. A statement error is returned as an entry and
stops the batch, so you must read every entry. Lexical errors and other errors
before execution throw or reject in the normal error format, and no statement runs.

Batches use the same semicolon-aware parser as Rust and the CLI, trigger bodies and
multiline documents included. A batch has no implicit transaction and takes no
parameters. Put explicit transaction statements in the script, and read the
transaction state of the last entry after a failure. Each result is encoded before
the next statement runs, so a result that cannot be encoded also stops the batch.
Neither a stop nor an encoding error undoes earlier work. Results stay in memory,
and limits for scripts and result memory need more qualification.

### Cancellation with AbortSignal

Pass `{ signal: controller.signal }` in the options position. If you skip an earlier
optional argument, pass `undefined` in its place.

| Async method | Call with options |
| --- | --- |
| Queries and row helpers | `execute(sql, parameters, options)`, `all(sql, parameters, options)`, `first(sql, parameters, options)`, `exactlyOne(sql, parameters, options)` |
| Query profiling | `profileSelect(sql, parameters, options)` |
| Collection integrity | `checkCollectionIntegrity(table, limits, options)` |
| Script batch | `executeBatch(script, options)` |
| Document export | `exportDocuments(table, format, options)` |
| Document import | `importDocuments(table, input, format, options)` |
| Database file copy | `exportFile(destination, options)` |
| Migrations | `migrate(plan, options)` |

For example, `await rql.exportDocuments('posts', undefined, { signal })` uses the
default JSON format. `open()` and `close()` do not take a signal.

Each signalled request gets its own native cancellation token. A queued request that
is aborted runs no SQL when its turn comes. Accepted requests keep their queue order
and their queue slot until the worker responds. Cancellation does not close the
database or cancel other requests.

Active cancellation is cooperative: the engine reacts at its progress points.
Completion can win the race with the abort, so use the returned result or error. An
abort alone does not prove a rollback. A cancelled query rejects with
`RDB_CANCELLED` and the usual transaction fields, not with `signal.reason`. There is
no fixed cancellation delay for compilation or work outside the engine. The pinned
defect in trigger interruption is still a release gate.

Listeners and token entries are released on a response, a send failure or a worker
failure. Tokens do not keep connections, so a late abort cannot reach another
request. The process-local registry allows 16,384 outstanding signalled operations
across all clients, in addition to the queue limits of each client. Requests without
a signal use the normal path. Synchronous `Database` methods do not take signals.

How each operation ends after cancellation:

- `executeBatch`: a request aborted before it starts rejects before the script is
  split. Cancellation during the batch appears as a final `RDB_CANCELLED` entry with
  the byte offset and transaction state of that statement. Earlier entries stay,
  and later statements do not run. Earlier writes keep their effects, and an explicit
  transaction can stay active. Coordinate any rollback yourself, and use a fresh
  signal for a retry.
- `importDocuments` and `exportDocuments`: rejects with `RDB_CANCELLED` and
  transaction fields. An import rolls back its own work and keeps earlier outer work.
  An export returns a complete string or an error.
- `migrate`: interrupted statements report `RDB_CANCELLED`, and the message keeps the
  migration version and byte offset. All pending scripts and history rows roll back
  together, and applied migrations stay. A request aborted before it starts does no
  database work, but JavaScript argument validation still runs.
- `close()`: runs the accepted requests and does not abort them. Signals can still
  cancel accepted operations while `close()` waits. Later requests reject with
  `RDB_CLOSED`.

Parsing, conversion and serialization have no fixed cancellation delay. In every
case, a successful completion can win the race, so read the outcome before a retry.

### Timeouts

Async operations that take options also take `timeoutMs`, an integer from 0 to
4,294,967,295:

```js
await rql.execute('SELECT * FROM posts', {}, { timeoutMs: 500 });
```

The deadline starts when the request token is made: after the parameters are
encoded and before the request enters the worker queue. Time in the queue counts,
and 0 has already expired. An expired request returns `RDB_CANCELLED` through the
cancellation path, with the normal rollback and transaction fields. You can pass
`signal` with it. An expired request does not cancel later requests. `close()` has no
timeout.

This is not a hard deadline for the Promise. The engine notices the deadline at its
progress points. Parsing, work outside the engine, cleanup and the response can take
longer, and completion can win the race. Synchronous clients do not have this
option.

## Result and memory limits

### Bounded SELECT results

Both clients have `selectWithLimits(sql, limits, parameters?)` and
`profileSelectWithLimits(sql, limits, parameters?)`. The `AsyncDatabase` versions
take `{ signal }` as a fourth argument. Both limits are required, nonnegative
`bigint` values:

```js
const result = await rql.selectWithLimits(
  'SELECT title FROM posts LIMIT 100',
  { maxRows: 100n, maxPayloadBytes: 65536n },
);
```

If the result passes a limit, the call fails with `RDB_LIMIT` and the transaction
fields, and it returns no partial rows or metrics. An empty result still counts its
column names. These calls accept one SQL SELECT, one-hop FETCH included. Fetched
documents and their duplicates count toward the payload limit, and missing targets
count as nulls. A found target is counted with its output multiplicity before the
cache keeps it, so the call can fail early. Reference placeholders do not count.
`execute` and `profileSelect` keep their own result policy.

Payload bytes count UTF-8 column names, strings, object keys, record table names and
string keys. Integer keys and numbers count eight bytes, `null` and booleans one byte,
and binary values and encoded vectors their byte length. Objects and arrays count
the sum of their contents. These are budgets for the logical result. They do not
count containers, allocator overhead, temporary decoding, copies in Node transport
or engine working memory. FETCH keeps its own reference and tagged-JSON limits, and
the final payload limit does not cover temporary references or the target cache.
These limits are not process memory limits or deadlines.

### Limits on write results

Both clients have `writeWithResultLimits(sql, limits, parameters?)`. The
`AsyncDatabase` version takes `{ signal }` as a fourth argument. It uses the same
required `bigint` fields `maxRows` and `maxPayloadBytes`:

```js
const result = await rql.writeWithResultLimits(
  'UPDATE posts SET published = true RETURNING id',
  { maxRows: 100n, maxPayloadBytes: 65536n },
);
```

Supported SQL writes and document writes run in an operation savepoint. Native SQL
counts each result row before the frontend keeps it. Collection SQL and document
RETURNING count each projected row before they keep it. Native destinations with
logical sources also count rows during the final collection. In all cases, a write
over a limit returns `RDB_LIMIT` and rolls back, index and trigger effects included.

This policy accepts or refuses a result. It does not bound engine RETURNING buffers,
write candidates, document snapshots or the working memory of one projection.
Transaction control, DDL, reads and multiple statements are refused. A write without
RETURNING can succeed with limits of zero, because affected rows are not returned
rows. An empty RETURNING still counts its column names. Cooperative cancellation
uses the usual error fields and the savepoint recovery, and the count of completed
results has no fixed cancellation delay.

### Collection write buffers

Both clients take an optional second argument when they open:

```js
const options = { writeBufferLimits: { maxRows: 10_000n, maxPayloadBytes: 64n * 1024n * 1024n } };
const rql = new Database(':memory:', options);
const worker = await AsyncDatabase.open(':memory:', options);
```

The limits apply to each write-candidate buffer and each document-snapshot buffer of
the frontend. If a buffer passes a limit, the operation fails with `RDB_LIMIT` and
rolls back, and earlier work in the transaction stays. Ordinary reads and the result
limits above have their own policies. The limits are off by default and cannot
change while the connection is open. Both fields are required, nonnegative uint64
`bigint` values. Unknown options and invalid values fail before the database opens.

These are not total memory limits. Native engine buffers, parsing, temporary values
and overlapping buffers are outside them, and transfer APIs keep their own limits.
See [result budgets](https://github.com/romedb/romedb/blob/main/romedb/docs/result-budgets.md) for the accounting and the open
resource gates.

## Profiling and audits

### SELECT profiling

Both clients have `profileSelect(sql, parameters?)`. It returns `{ result, metrics }`.
`result` has the same typed rows, `bigint` affected count and transaction fields as
`execute`.

```js
const profile = await rql.profileSelect(
  'SELECT id FROM posts WHERE author = $author',
  { $author: new Record('people', 'alice') },
);
console.log(profile.result.rows, profile.metrics.rowsRead);
```

Every metric is a `bigint`: `rowsRead`, `rowsWritten`, `fullscanSteps`,
`indexSteps`, `vmSteps`, `sortOperations`, `btreeSeeks`, `fetchBatches`,
`fetchRowsRead` and `fetchVmSteps`, and the graph and spatial counters of
`QueryMetrics`. The addon sends counters as decimal strings, so no JavaScript
`Number` conversion loses digits.

The `fetch` counters report the target SELECT batches, physical row reads and VM
instructions of FETCH, and they are zero without target reads. The other counters
cover the main statement. References that point to the same target share its reads.
Physical row reads are not logical document counts. Metadata and lowering queries,
decoding and transport are not counted.

`profileSelect` accepts one SQL SELECT. Writes, multiple statements, EXPLAIN and
direct-record shorthand are refused. A failure throws or rejects with the usual
error fields and no partial counters. The `AsyncDatabase` version takes
`{ signal }` as a third argument, which cancels cooperatively and is not a deadline.

### Collection integrity audit

Both clients have `checkCollectionIntegrity(table, limits?)`. It returns the
`bigint` counts `documents`, `indexes`, `indexEntries` and `encodedBytes`, and the
transaction fields:

```js
const audit = await rql.checkCollectionIntegrity('posts', {
  maxDocuments: 100_000n,
  maxEncodedBytes: 64n * 1024n * 1024n,
});
console.log(audit.documents, audit.indexEntries);
```

The optional limits are nonnegative `bigint` values that fit in uint64. Without them,
the defaults are 100,000 documents and 64 MiB of encoded IDs and documents. Zero is
valid for an empty collection. The `AsyncDatabase` version takes `{ signal }` as a
third argument.

The audit tests typed IDs, validation and the consistency of index entries in one
snapshot, and it repairs nothing. `RDB_LIMIT` returns no partial report. The limits
do not bound engine memory or time. This is not a test of physical pages or B-trees,
and it is not a tool to recover from corruption.

## Typed vectors

The vector factories work without a database, and both clients accept their values:

```js
const { Vector } = require('@romedb/node');
const dense = Vector.float32([1, 0, -1]);
const precise = Vector.float64(new Float64Array([0.1, 0.2, 0.3]));
const sparse = Vector.sparse32(new Float32Array([1, 0, -1]));
const quantized = Vector.quantized8([1, 0, -1]);
const bits = Vector.bit1([1, 0, -1]);
const sparseEntries = Vector.sparse32Entries(65536, [[0, 1], [65535, -1]]);
```

The five dense factories take a number array (a readonly array in TypeScript), a
`Float32Array` or a `Float64Array` of 1 to 65,536 finite numbers. `float64` keeps
binary64 values. The other factories first convert to float32 and refuse an overflow
to infinity. Quantized and bit conversion lose precision. Inputs are copied, and the
factories run synchronously. The installed-package smoke uses the five dense
factories and the sparse-entry factory through both clients.

`Vector.sparse32Entries(dimensions, entries)` takes `[index, value]` pairs. The
exported `SparseVectorEntry` type supports readonly tuples. Dimensions are 1 to
65,536. Indices must be unique, strictly increasing and in range, zero-valued entries
included. Values must be finite and fit in float32. Zero entries are left out after
the float32 conversion, and no entries make an all-zero vector of the given
dimensions. Memory grows with the count of entries, with no dense copy.

Invalid indices throw a `RangeError`, and malformed pairs throw a `TypeError`. Invalid
container or component types throw a `TypeError`, and invalid dimensions or a float32
overflow throw a `RangeError`. Native conversion errors are ordinary errors with no
transaction fields, because no connection is open. `new Vector(encodedBytes)` keeps
the encoded-byte path, and binding that value still validates its encoding.

The native adapter uses the Rust value constructors and validates their output.
JavaScript passes a bounded binary64 buffer, so no JSON number conversion changes a
value. Tests cover float32 halfway rounding, subnormal underflow and signed zero with
known IEEE-754 bits. `float64` keeps the smallest positive and negative subnormals, a
value next to one and the largest finite binary64 value. Quantized conversion can fail
for finite inputs: the pinned scale calculation overflows for a range from the
negative to the positive float32 maximum. These errors happen locally and do not
change an open transaction.

## Embedded client

`@romedb/node/embedded-client` opens a local MVCC file. It has the same request
methods as the URL client (`info`, `read`, `prepareWrite`, `submit`, `recover` and
`writeAtomic`) and the same value types. It needs the native addon. CommonJS
and ESM imports both work.

```js
const { openEmbedded, Record } = require('@romedb/node/embedded-client');
const rql = await openEmbedded({ path: 'app.romedb', create: true });
try {
  await rql.writeAtomic([
    { sql: "INSERT INTO posts {id: posts:hello, title: 'Hello', visits: 1}" },
    { sql: 'UPDATE posts:hello MERGE {visits: $visits}', params: { $visits: 2n } },
  ]);
  const { results } = await rql.read([
    { sql: 'SELECT title, visits FROM posts WHERE id = $id', params: { $id: new Record('posts', 'hello') } },
  ]);
  console.log(results[0].rows); // [ [ 'Hello', 2n ] ]
} finally {
  await rql.close();
}
```

### Opening and creating files

`openEmbedded({ path })` opens an existing regular file with one link. To create a
new file, pass `create: true`. Creation refuses an existing file and leftover
journal files. Files use MVCC, the journal mode of `romedb serve` and RomeDB Cloud.
A file moves between these without conversion and gets the same write conflicts.

Opening a WAL file fails with `RDB_UNSUPPORTED` and leaves the file unchanged. Stop
its owner and convert it with `romedb migrate-to-mvcc`. The managed connection turns
on SQL foreign keys. The legacy APIs keep their own setting.

### Private files

`create: true` makes the database file with mode 0600. The WAL file (`NAME-wal`) and
the MVCC log (`STEM.db-log`, which holds row data until a checkpoint) get the
permissions of the database file, as in SQLite. RomeDB creates them before the
engine does. On each open, it removes permission bits that the database file does
not have from existing ones. So every file of a new database is 0600, and a database
made by 0.2.11 or earlier gets private journal files at its next open.

### Open files

One process keeps at most `maxOpen` different files open: 1 to 1024, 16 by default.
An open over this number fails with `RDB_LIMIT`. Clients of a file that is already
open do not count again. To keep many databases, such as one per site, open them
with the same `maxOpen` and close the least recently used one yourself.

`close()` of the last client checkpoints the MVCC log into the database file, so the
next open has no log to replay. Then it releases the OS lock of the file. Reopening a
closed file takes about 75 to 90 ms on Linux x64 (release build, 2026-11-02), nearly
independent of its size. Each open file has its own worker thread, and starting it
and loading the addon in it takes about 60 ms of that. The engine opens in about
9 ms, and closing with the checkpoint takes about 10 ms.

### Batches and sequences

Clients of the same canonical file in one Node module share one native worker. Each
batch runs as one worker operation inside a managed transaction. All statements are
validated before any runs, and user transaction control is refused. A failure or an
exceeded result or work limit rolls back the whole batch. The `executeBatch`
behavior of the legacy API does not change.

Sequence conditions cover writes through this shared adapter. Its capabilities
report `sequenceScope: 'adapter'` and `replayDurability: 'adapter-lifetime'`. Other
native or legacy handles do not advance its sequence. An application must use one
managed adapter for each canonical file. To share a file between processes, use the
service URL.

`info()` also returns `engineVersion`, the RomeDB release of the native engine.
Database URLs report it from local services 2.7.0 and later and from Cloud. Older
services do not report it.

### Write recovery

The adapter keeps 64 exact write receipts in memory. A saved journal can recover a
write while that adapter stays open. After its last client closes, reopening makes a
new identity and refuses old requests. After a reopen, reconcile uncertain writes
from the stored data. Service and Cloud connections recover writes across process
restarts.

### Limits and closing

Each adapter accepts 32 outstanding requests and 2 MiB of queued request bodies, and
a process has 16 open file adapters by default. The request, result and work limits
match the common contract. Calls time out after 20 seconds by default and accept a
`timeoutMs` of 1 to 120,000. Opening also has a deadline. If opening expires, cleanup
continues until the native startup returns. Filesystem work and native cleanup do
not stop early.

`close()` stops requests from that client and cancels its outstanding work. The last
client runs the remaining worker queue, closes the native handle and waits for the
worker to exit. Await `close()` of every client to release the file.

### Copying a database into a file

`exportFile` copies the open database into a new `.romedb` file while reads and
writes continue. The file is one snapshot, and it opens by itself in `openEmbedded`
and `romedb serve FILE`.

```js
const { sequence, bytes } = await rql.exportFile('backups/site-2026-11-03.romedb');
// Or stream the copy, for example as a download. Nothing stays on disk afterwards.
const { size, body } = await rql.exportFile({ signal });
```

The file holds every write up to and including `sequence`, and none after it. The
destination and its `-wal`, `-shm`, `-log`, `-journal` and `.db-log` files must not
exist, or the call fails with `RDB_ALREADY_EXISTS`. The new file has mode 0600 and
appears only when it is complete.

The streaming form builds its copy in a private folder beside the database file. It
removes the name of the copy before it returns, so read `body` to the end or cancel
it. Exports have no deadline unless you pass `timeoutMs`. `signal`, `timeoutMs` and
`close()` cancel a running export. It then rejects with `RDB_CANCELLED`, unless it
finished first.

#### Leaving data out of a copy

A whole copy holds everything, including sessions and tokens. Use `emptyTables` when
someone else gets the file:

```js
await rql.exportFile('site.romedb', { emptyTables: ['cms_auth_sessions', 'cms_cache'] });
```

- The named tables keep their schema and indexes, but no rows.
- Their rows are never read, so they cannot reach the file. They are not in freed
  pages, a log or a receipt.
- A name that is not a table fails with `RDB_VALIDATION`.
- An edge table must be emptied together with the tables it connects.
- Rows in other tables keep their references to the emptied rows.
- The copy also gets a new identity, because a receipt can hold row data.

Use `newIdentity: true` to copy every row into a new database, for example to
duplicate a site. The copy leaves out the database ID, the sequence and the receipts.
Its sequence starts at 0, and it gets a new ID the first time it opens.

The URL client has the streaming form, `rql.exportFile({ signal, timeoutMs, waitMs,
emptyTables, newIdentity })`, for `romedb serve`, `serve --directory` and Cloud. On Cloud, a
job builds the file, and the client waits for it and then downloads it, so a large database
can take minutes. `waitMs` bounds that wait, 3 hours by default, and `timeoutMs` covers
each request. See
[database files](https://github.com/romedb/romedb/blob/main/romedb/docs/service-protocol.md#database-files) for how the copy is
made.

## URL client

`@romedb/node/client` connects to a database behind `romedb serve`, `romedb serve
--directory` or RomeDB Cloud. Importing it never loads the native addon.

```js
const { connect, Float64, saveRequest, loadRequest } = require('@romedb/node/client');
const rql = await connect({ url: process.env.ROMEDB_URL, token: process.env.ROMEDB_TOKEN });
const snapshot = await rql.read([{ sql: 'SELECT * FROM posts LIMIT 20' }]);
const request = rql.prepareWrite([
  { sql: 'UPDATE posts:p1 MERGE {score: $score}', params: { $score: new Float64(1) } },
], { ifSequence: snapshot.sequence });
await saveRequest('/private/new-request.json', request);
try {
  await rql.submit(request);
} catch (error) {
  if (error.outcome === 'unknown') {
    // Keep this journal. Recovery sends its original identity and exact body.
    await rql.recover(await loadRequest('/private/new-request.json'));
  } else throw error;
}
await rql.close();
```

### Saved requests

Use a new journal file name for each prepared write. `saveRequest` refuses to
overwrite an existing file, and it flushes a mode-0600 file and its folder. A journal
holds SQL and parameter values, so keep it private, although it holds no token. A
refused recovery does not tell the outcome of the earlier write. Do not replace an
expired or uncertain request with a new write automatically.

### URLs and tokens

A local service URL is `http://127.0.0.1:4321`. A Cloud URL names the organization
and database by stable UUIDs, and a dashboard URL that ends in `/query` also works.
Use HTTPS for every host that is not local.

`romedb://` is another name for `https://`. The client replaces the scheme and applies
the same rules, so `romedb://cloud.romedb.com/v1/organizations/{org}/databases/{db}`
and the `https://` form are the same database. Requests use `redirect: 'manual'`, and
any redirect fails with `RDB_PROTOCOL`, so the token never goes to another URL. Pass
the bearer token separately. The client refuses a token in the URL.

### Cloud URLs

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

### Local directories

An organization URL on a loopback host, such as a database of `romedb serve
--directory` (`http://127.0.0.1:4321/v1/organizations/{org}/databases/{db}`), always
fetches the details at connect. A reply with `serviceVersion: 1` is a local service.
The client then negotiates its capabilities and uses the 64 KiB service request
format, with no warm request. Any other reply is Cloud running locally, which keeps
the Cloud request format. So an application can use the same organization URLs for a
local directory and for Cloud.

### Values and results

Results use positional rows and keep duplicate columns. Integer results are `bigint`.
Number results are `Float64` instances with a `.value` number. They keep their exact
float type when you bind them again, integral floats and negative zero included.

This entry exports `Record`, `Vector` and the value codec of `@romedb/client`, which
`@romedb/node/embedded-client` also uses. The main `@romedb/node` entry has its own
`Record` and `Vector`, so do not pass those to this client. Encoded vectors and the dense float32, float64 and sparse-entry factories
work without native code. `null` is a present field. `undefined` and array holes are
refused, so no data disappears without an error.

### Errors and timeouts

Requests and responses have finite byte and row limits. Each call times out after 20
seconds by default and accepts `signal` and `timeoutMs`. An HTTP error reply with a
`Retry-After` header sets `ClientError.retryAfterMs`. A transport or response failure
without a database reply keeps the underlying error as `ClientError.cause`, for
example a `fetch failed` TypeError with `ECONNREFUSED`.

`rql.sequenceKnown` is `true` once a reply has given the current sequence. `rql.cloud`
tells a Cloud database from a local service. A write cancelled after it was sent stays
unknown until a receipt confirms it. Cloud capabilities are marked as documented,
because its API does not negotiate the local capability object. See the
[service contract](https://github.com/romedb/romedb/blob/main/romedb/docs/service-protocol.md) for limits, receipt rules and
compatibility gaps.

### Query versions

From RomeDB 2.9.0 the client asks Cloud for query version 3
(`x-romedb-query-version: 3` on `/query` and `/read`). In version 3, Binary and
Vector values travel as canonical base64, and request bodies can be up to 1 MiB.
Local services and embedded databases keep version 2, with byte arrays and 64 KiB
bodies.

Version 2 covers tagged Record keys and typed query errors. Resource URLs stay `/v1`,
and `readVersion: 2` separately defines fresh-read execution. The client accepts the
version it asked for, version 2, and older responses without the header, and it
decodes both byte forms. An unknown version fails with `RDB_PROTOCOL`. A write that
was sent then stays `unknown` and keeps its exact saved request for recovery.

Saved writes are version 2 and record their query version. Version 1 saved writes
from 2.8.0 replay at version 2. Older receipts with enum keys still decode.

### Workers, browsers and other runtimes

From 0.2.12, `@romedb/node/client` and `@romedb/node/values` use only Web APIs:
`fetch`, `crypto.randomUUID`, `TextEncoder`, `AbortSignal`, `atob` and `btoa`. Node.js
loads the CommonJS files through the `node` export condition. Every other runtime and
bundler gets the ES modules `client.mjs` and `values.mjs`, for example Cloudflare
Workers, browsers and Deno with a bundler. They run the same code: `build-esm.cjs` in
`@romedb/client` writes them from `client.cjs` and `values.cjs`, and the package tests
fail when they are out of date.

Workers need no `nodejs_compat`. The client calls `fetch` without a `this` value, so
`globalThis.fetch` and your own `fetch` both work in Workers.

```js
import { connect } from '@romedb/node/client';

export default {
  async fetch(request, env) {
    const rql = await connect({ url: env.ROMEDB_URL, token: env.ROMEDB_TOKEN });
    const { results } = await rql.read([{ sql: "SELECT title FROM posts WHERE array::contains(tags, 'news')" }]);
    return Response.json({ posts: results[0].rows.length });
  },
};
```

`@romedb/node/values` exports only the value types and the codec (`Float64`, `Record`,
`Vector`, `encodeValue`, `decodeValue`). Use it for code that never connects, such as
a browser bundle. Both entries share one set of value classes.

Binary values and `Vector.bytes` are plain `Uint8Array` objects in every runtime.
Before 0.2.12, Node returned `Buffer`. Use `Buffer.from(bytes)` where you need Buffer
methods. `saveRequest` and `loadRequest` need Node.js file access and reject in other
runtimes. There, store the `PreparedWrite` object yourself, for example in a Durable
Object.

The package also contains the native addon. So installing it for a Workers app also
downloads the addon, but the Workers bundle never includes it. To skip that download,
install [`@romedb/client`](https://www.npmjs.com/package/@romedb/client). It holds this
code: `@romedb/node` depends on it, and `@romedb/node/client` and `@romedb/node/values`
pass it through. A project that uses both packages gets one set of value classes when
both are at the same version.

## Load errors

On Node.js versions before 22, importing the native entries throws
`RDB_RUNTIME_VERSION` before native code loads.

If the native addon is missing or cannot load, importing the package throws
`RDB_NATIVE_LOAD`. The message names the platform, the architecture and the Node
version, and it points to the source build instructions. `error.cause` keeps the
original loader error, for example a missing library or an invalid binary. The package
does not select, download or rebuild an addon automatically.

## For maintainers

### Build and test from the checkout

From the repository root, install the declaration checker and run the checks:

```sh
pnpm --dir romedb/bindings/node install --ignore-scripts
romedb/scripts/check.sh
```

`check-node.sh` builds the addon and copies it to the ignored `romedb.node` path. Then
it runs the Node tests and type-checks the declarations. If you need the temporary
toolchain, use the documented Rust environment of the repository. The package has no
npm runtime dependencies. TypeScript 5.8.3 is pinned only for the declaration checks.

### Local package smoke

Normal npm packing first makes sure that the native addon loads. A missing or
incompatible build fails before the archive exists. The npm file list holds the native
addon, the synchronous and worker JavaScript entries, the declarations, the README,
the MIT license and the third-party notices. Build the addon for the current
platform, then test the actual tarball from the repository root:

```sh
romedb/scripts/check-node.sh
node romedb/scripts/check-node-package.cjs
```

The smoke packs into a temporary folder and compares the exact file list. It installs
the tarball into a separate consumer with npm offline and lifecycle scripts off. Then
it tests public synchronous and worker queries, typed int64 and record values,
rollback and reopen. It also covers profiling and audits in both clients, cancellation
before dispatch for every signalled API, and listener and token cleanup. It compiles a
consumer TypeScript import against the installed declarations. It removes its
temporary files afterwards.

For an optimized local addon, run `romedb/scripts/check-node.sh --release` before the
smoke. That build uses the pinned release profile of the workspace and runs the same
tests and TypeScript type check. It does not build universal prebuilds or publish a package.
On Linux, `--release-stripped` also runs `strip --strip-debug` on the package copy. It
keeps the full release artifact in the Cargo target folder for diagnostics, and it
needs `strip`.

The smoke is a maintainer test, separate from routine CI, because the debug addon is
large. The tested local artifact is Linux x64 with Node 24.19.0, not a universal
binary. Prebuilds for other platforms, more Node versions and platforms, and complete
distribution notices are release work. Releases reach npm through the publishing
workflow, which [publishing to npm](https://github.com/romedb/romedb/blob/main/romedb/docs/npm-publishing.md) describes.

### Dependency declaration inventory

From the checkout root, with the pinned Rust toolchain:

```sh
node romedb/scripts/inventory-node-dependencies.cjs x86_64-unknown-linux-gnu romedb/docs/node-dependencies-linux-x64.json
node romedb/scripts/inventory-node-dependencies.cjs x86_64-unknown-linux-gnu romedb/docs/node-dependencies-linux-x64.json --check
```

This offline, locked Cargo query records normal and build dependencies, their declared
license expressions, the target and the lockfile hash. The Linux inventory has 192
package versions. Build tools are included, and development dependencies are not.

`cfg_block` 0.1.1 has no license expression in the query output. Its pinned manifest
declares `license-file = "LICENSE"` instead, and that file contains an Apache 2.0
notice. The package notice file includes it and the Apache 2.0 text. The inventory
keeps the missing expression rather than an inferred value.

The inventory is an input for an audit. It does not prove that every listed crate is
linked into the addon, and it is not a complete component inventory or a replacement
for license texts. Bundled C and C++ sources and other vendored components need their
own inspection. Regenerate the inventory when dependencies change, and qualify other
targets separately.

With Python 3.11 or later and the same Cargo cache, inspect the cached source archives:

```sh
python3 romedb/scripts/audit-crate-notices.py romedb/docs/node-dependencies-linux-x64.json romedb/docs/node-crate-notices-linux-x64.json
python3 romedb/scripts/audit-crate-notices.py romedb/docs/node-dependencies-linux-x64.json romedb/docs/node-crate-notices-linux-x64.json --check
```

The audit compares archive checksums with `Cargo.lock`. It records declared license-file
paths and hashes of files whose names look like licenses or notices, nested bundled
sources included. It reads the archives without extracting them. Missing or ambiguous
archives, checksum mismatches and stale reports fail. The current report has 185
archives with 316 candidate files. 13 archives have no candidates, and seven workspace
packages need separate inspection. A name search can miss inline notices and unusual
names, and a found file does not prove that the package has every required notice.
The report stays outside the npm package.

For archives without candidates, the report also searches selected code and text files
of up to 200,000 bytes for copyright, SPDX and license phrases. It records source
hashes, line numbers and short excerpts. The current report finds four files in
crc32c, rapidhash and rquickjs-core: crc32c refers to zlib, and rquickjs-core has an
attribution to the futures authors. These references need review. Ten of the thirteen
archives have no hits in this bounded search, and no hit is not proof that no
attribution is needed.

Generate and test the collected crate notice texts:

```sh
python3 romedb/scripts/bundle-crate-notices.py romedb/docs/node-dependencies-linux-x64.json romedb/docs/node-crate-notices-linux-x64.json romedb/bindings/node/THIRD_PARTY_CRATE_NOTICES.md
python3 romedb/scripts/bundle-crate-notices.py romedb/docs/node-dependencies-linux-x64.json romedb/docs/node-crate-notices-linux-x64.json romedb/bindings/node/THIRD_PARTY_CRATE_NOTICES.md --check
```

The generator compares the lockfile, inventory, archive and notice-file hashes again
before it writes. It writes identical texts once, with source links for each package.
The npm package includes this partial collection: 125 distinct texts for 316 candidate
files. The offline package smoke compares both installed notice files exactly. The
twenty packages without collected candidates and any other inline or bundled
attribution are still review items, so this is not a complete set of distribution
notices.

Run the synthetic tests of the notice generator without Cargo or registry access:

```sh
python3 romedb/scripts/test-notice-bundle.py
```

They test these cases:

- Reproducible output.
- Identical texts written once, with their source references.
- Detection of a stale inventory or a stale output.
- A package identity mismatch, a corrupt archive and changed notice hashes.

A failure case must leave an existing output file unchanged.

### Linux artifact requirements

Inspect the package addon without loading it (Python 3.11 or later and `readelf`):

```sh
python3 romedb/scripts/inspect-node-elf.py romedb/bindings/node/romedb.node romedb/docs/node-elf-linux-x64.json
python3 romedb/scripts/inspect-node-elf.py romedb/bindings/node/romedb.node romedb/docs/node-elf-linux-x64.json --check
```

The report names the tested stripped artifact by size and SHA-256. It records the ELF
class and machine, linked libraries, search paths and required symbol versions. This
artifact uses GLIBC symbols up to 2.35 and has no RPATH or RUNPATH. A passing local
Linux test does not prove compatibility with older glibc systems, musl distributions,
another architecture or another Node version. A release baseline needs an explicit
build environment and tests on the advertised targets. Regenerate the report whenever
the addon changes.
