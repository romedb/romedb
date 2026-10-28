# RomeDB service and shared client contract

Version 1. Defined 2026-10-11 for the Studio implementation. This document is a
contract shipped in RomeDB 2.3.0. The existing
2.2.0 embedded APIs and script-batch semantics remain unchanged.
Cloud query/read responses separately advertise `x-romedb-query-version: 2` for
SDK-tagged Record keys and confirmed typed errors. Resource URLs remain `/v1`;
`readVersion: 2` defines fresh-read execution. The SDK rejects unknown advertised
response versions and preserves dispatched writes as uncertain for exact recovery.

## Commands and ownership

The new public executable is `romedb`; keep `romedb-cli` as a compatibility name.
Both names use the same CLI implementation and preserve existing commands.

```sh
romedb serve app.romedb --port 4321
romedb serve new.romedb --create
```

`serve` runs in the foreground, owns one file, and defaults to `127.0.0.1:4321`.
Opening a missing file fails unless `--create` is explicit. Creation fails if the
file already exists. New service databases use MVCC. Existing WAL files fail with
migration guidance. They are never converted during an ordinary open. The explicit
`migrate-to-mvcc SOURCE OUTPUT_DIRECTORY --backup BACKUP_DIRECTORY` command preserves
the source and verifies a private backup before converting a working copy.
`restore-migration BACKUP_DIRECTORY RECOVERY_DIRECTORY` restores that verified
backup to a new WAL database. See the [migration procedure](service-migration.md)
for audit limits, failure handling, and the paired main-file/journal publication.

An application and Studio connect through the service when they share a live
file. Embedded access remains available when one application owns the file.
No multiprocess WAL or embedded default change is part of this contract.
Service opens require a regular file with one link. Reject symlink and hard-link
aliases before opening the engine, because an alias can select a different journal.

The service creates a random bearer token in a file readable only by its owner.
Startup prints the database URL and token-file path, never the token. Reuse the
protected token on restart. Reject insecure token files, symlinks and accidental
replacement of existing credentials. Require an explicit token-file override for
managed credentials. Never put a token in a URL or ordinary log.

## URL and authentication

Use `Authorization: Bearer <token>` for all database endpoints. No cookies or
browser origins are accepted by the database service. Studio connects from its
Node backend. The service binds only to loopback in this release.

After discovering the database ID, the shared client sends
`X-RomeDB-Database-ID: <id>` on subsequent requests. Prepared writes use the ID
saved with that request. The local service rejects a mismatched or repeated ID
header before reading the body or executing SQL. This protects saved requests
when another database replaces an owner at the same port, even with a reused token.
Raw local callers can omit the header when they intentionally target the current
owner. Callers that retain requests across sessions must send the saved ID.
Cloud database identity is also bound by its stable UUID route.

| Configuration | Normalized database URL |
| --- | --- |
| `http://127.0.0.1:4321` | `http://127.0.0.1:4321/v1/database` |
| Local full URL | `http://127.0.0.1:4321/v1/database` |
| Cloud full URL | `https://cloud.romedb.com/v1/organizations/{org}/databases/{db}` |
| Cloud dashboard query URL | Same scoped URL with its final `/query` removed |
| `romedb://` URL | The same URL with `romedb://` replaced by `https://`, then normalized as above |

Accept only HTTPS (or its `romedb://` alias) or HTTP on explicit loopback hosts. Reject URL credentials,
query strings and fragments. Do not follow redirects with a database credential.
Versioned paths are mandatory; do not infer organization/database identity from
unverified token claims. Cloud database JWTs require the stable UUID route.

GET the database URL for its stable `id` and current `sequence`. POST `/read` and
`/query` relative to that URL. Local details additionally report `serviceVersion:1`,
capabilities, finite limits, and the journal mode. Local services from 2.7.0 and
Cloud also report `engineVersion`, the RomeDB release of the engine; a client must
accept details without it from older services. Cloud details currently have no
matching capability object: the client reports the documented Cloud contract,
with capability provenance `documented`, not `negotiated`. It must still preserve
real authorization failures and server limits.

Cloud database URLs also accept POST `/warm` with an empty JSON object and a
credential that can read the database. Cloud starts the database's container and
opens the database without running a query, and replies at once with
`{"state":"ready"|"starting"|"skipped"}` (`starting` uses status 202). `skipped`
means Cloud declined because another database is busy or was used in the last
five seconds; a warm request never replaces such a database. Local services have
no `/warm` route.

## Shared asynchronous interface

The new Node entry is `@romedb/node/client`. Importing it does not load the native
addon or create a worker. Native `@romedb/node` imports retain their current API.

```ts
const db = await connect({ url, token });
const snapshot = await db.read([{ sql: 'SELECT * FROM posts LIMIT 20' }]);
const write = db.prepareWrite([
  { sql: 'UPDATE posts:p1 SET title=$title', params: { $title: 'Edited' } },
], { ifSequence: snapshot.sequence });
const result = await db.submit(write);
await db.close();
```

`info()` refreshes details. `read(statements, options)` executes a fresh snapshot.
`prepareWrite(statements, options)` produces a serializable immutable request
before sending. `submit(request, options)` and `recover(request, options)` send
the exact saved body to the exact database URL. `writeAtomic` is the convenience
prepare-and-submit form and includes that request in every uncertain error.
`close()` stops new requests and cancels outstanding transport work.
When RomeDB Cloud refuses a request with `503` and a `query_queue_*` code, the
request was never dispatched. The client waits for `retry-after` (at most 2 s,
within the request timeout) and sends it once more; any other `503` is returned.

When the local service process stops cleanly, it finishes admitted requests and then
checkpoints its MVCC log into the database file, so the next open has nothing to
replay. A fenced service skips that checkpoint and keeps the log for recovery. While
the service runs, the engine checkpoints automatically when the log passes about
4 MB, so the log does not grow without bound between restarts.

Each statement has SQL and an optional typed `params` map. Accept 1–32 statements.
Each SQL field contains one statement; the backend validates it with the RomeQL
parser. The Studio editor uses the parser to produce the statement list. There
are no open transactions spanning HTTP requests. Do not rename the existing
non-atomic `executeBatch` operation or give it implicit atomic behavior.

The native-only entry `@romedb/node/embedded-client` exports
`openEmbedded({ path })` and the same client types. It implements the new methods
with one operation on one native worker connection per batch. It enforces the same value/row limits
and rejects user transaction control inside managed operations. It requires no
token or service. Embedded recovery is scoped to that adapter and its lifetime;
cross-process durable replay belongs to the service and Cloud.

Opening requires an existing file unless `create: true` explicitly creates one.
Local service request connections also enable SQL foreign keys before beginning
their transaction and report `foreignKeys: true` in capabilities. Cloud query
connections also enable foreign keys before BEGIN; immediate/deferred rollback
and the hosted common fixture pass qualification.
The adapter preserves its journal mode and enables foreign keys on its managed
connection. Clients for the same canonical file in one Node module share a worker
and revision. Other native handles do not advance that revision. Capabilities
state `sequenceScope: 'adapter'` and `replayDurability: 'adapter-lifetime'`.
The last client's close ends the replay lifetime. Reopening creates a new
identity and rejects old prepared requests before executing SQL.

## Values and results

Use the existing document-transfer-v1 tagged value encoding. User objects that
contain `type` or `value` fields remain ordinary objects inside an `Object` tag.

| Logical value | Wire value |
| --- | --- |
| Null | `{"type":"Null"}` |
| Boolean/string | Their named tag with the JSON scalar in `value` |
| Signed int64 | `{"type":"Integer","value":"9223372036854775807"}` |
| Finite float64 | `{"type":"Number","value":"3ff0000000000000"}` |
| Record | `{"type":"Record","value":{"table":"posts","key":{"type":"String","value":"p1"}}}` |
| Binary/vector | Their named tag with encoded byte integers in `value` |
| Array/object | Their named tag with recursively tagged contents in `value` |

Integer strings are canonical decimal int64; Number strings are 16 lowercase
hexadecimal IEEE-754 bits, preserving negative zero. The new client represents
Number results as explicit `Float64` values so integral floats retain their type
when edited and rebound. `bigint` represents Integer; strings never become records
implicitly. `Record` and `Vector` are explicit client types. Byte buffers are
copied on construction. Remote vectors can use encoded bytes without loading the
native addon; dense float32/float64 and sparse-entry constructors are also pure JS.
The backend remains responsible for validating the complete vector representation.

Object field absence means missing. Null is an explicit present value. Reject
`undefined`, array holes, non-finite numbers, out-of-range integers, unsupported
objects and nesting deeper than 64. Editing a document must not stringify typed
values into strings. Float64 values must be explicitly unwrapped for calculations.

Results contain `columns: string[]`, positional `rows: Value[][]`, and decimal
uint64 `affected` (decoded to bigint). Preserve duplicate column names and require
row width to match column count. Do not convert rows into keyed objects.

## Reads, writes and sequence conditions

Fresh read request:

```json
{"readVersion":2,"requestId":"example_read","afterSequence":12,"statements":[{"sql":"SELECT title FROM posts WHERE id=$id LIMIT 1","params":{"$id":{"type":"Record","value":{"table":"posts","key":{"type":"String","value":"p1"}}}}}]}
```

Read reply:

```json
{"readVersion":2,"requestId":"example_read","sequence":12,"results":[{"columns":["title"],"rows":[[{"type":"String","value":"Original"}]],"affected":"0"}]}
```

Every `/read` invocation uses a fresh, read-only transaction. Retrying may observe
newer data. `afterSequence` is a causal lower bound, not a historical snapshot.
Reject writes, transaction control and `ifSequence` before executing any statement.
Read success requires the matching request ID, readVersion 2 and sequence at least
the requested lower bound. Reads never advance the publication sequence.

Atomic write request:

```json
{"requestId":"example_write","afterSequence":12,"ifSequence":12,"statements":[{"sql":"UPDATE posts:p1 SET title=$title","params":{"$title":{"type":"String","value":"Edited"}}}]}
```

`/query` executes all statements atomically, including conversion and result-budget
checks. Reject user BEGIN/COMMIT/ROLLBACK, host-file operations, extensions, triggers
and arbitrary PRAGMA. `ifSequence` compares the current publication sequence
atomically with the mutation. A stale edit fails without changing the database.
This conservative condition can reject an edit after an unrelated write too.

Each published write request increments the sequence once. A SELECT sent through
`/query` also gets a receipt and advances the sequence. Accept only safe-integer
sequences below `Number.MAX_SAFE_INTEGER` for requests. A confirmed write reply has
the matching ID and a sequence strictly above `afterSequence`, at most 64 positions
ahead. Remember the maximum confirmed sequence; out-of-order responses cannot move
the client backwards.

## Durable replay and unknown outcomes

Retain the latest 64 published request receipts. Store the exact request identity,
request-body hash, outcome and result with the committed mutation. Local replay
metadata belongs in protected RomeDB storage and is committed atomically with the
write; a separate sidecar receipt cannot establish that invariant.

Replay an identical request before checking its optional sequence condition.
Changed-body reuse of a retained ID is a conflict. A previously unrecorded request
can execute only when `0 <= current - afterSequence < 64`; a request ahead of the
database also fails. This rule prevents an evicted request from executing again.
Database ID, sequence and receipts survive a service restart.

Save the request before dispatch if recovery across a client restart is required.
The journal contains version, canonical database URL and exact JSON body, never a
token. Write it exclusively with mode 0600, flush its contents and directory, and
refuse overwriting an existing journal. A journal is still private: it contains SQL
and parameter values. Load it with finite size/depth and envelope validation.

After dispatch, a timeout, cancellation, connection loss, malformed response or
HTTP rejection without a valid receipt leaves a write outcome `unknown`. Keep the
same ID, sequence bounds and exact body. Do not generate a replacement automatically.
A rejected recovery attempt does not prove that an earlier attempt rolled back.
If the receipt window expires, require reconciliation before making a new write.

Local and Cloud confirmed failures include ID, publication sequence and a typed
error receipt after all SQL effects are rolled back. Cloud retains zero-work stale
rejections and converts historical error strings at the confirmed public boundary
without rewriting stored receipts. Generic HTTP/network errors still leave a
write unresolved; a client cannot manufacture receipt identity or rollback.

## Finite budgets and errors

| Budget | Version-1 local default | Existing Cloud contract |
| --- | --- | --- |
| Request JSON | 64 KiB | 64 KiB |
| Statements | 1–32 | 1–32 |
| Rows per result | 1,000 | 1,000 |
| Aggregate result JSON | 256 KiB | 256 KiB |
| Client response envelope | 512 KiB | Client bound; includes metrics and envelope |
| Active requests | 4 | Bounded native admission; hosted admission also applies |
| Waiting requests | 32 | Hosted admission/quotas can reject |
| Database connections | 5: four admitted requests plus the publication connection | Hosted owner manages its connections |
| HTTP connections | 64, with at most 36 database jobs | Hosted admission/quotas also apply |
| Header/body read deadline | 5 seconds per phase | Hosted transport limits also apply |
| Execution deadline | 15 seconds, cooperative engine cancellation | 15-second native process limit |
| Client request timeout | 20 seconds, maximum configurable 120 seconds | Same client default |
| Receipt retention | 64 positions | 64 positions |
| Checked row reads per request | 1,000,000 | 1,000,000 |
| Checked row mutations per request | 100,000 | 100,000 |
| Checked VM steps per request | 10,000,000 | 10,000,000 |
| Frontend write buffer | 10,000 rows and 64 MiB per buffer | Existing hosted budgets also apply |

Reads and writes accept optional `workLimits` with nonnegative safe-integer
`rowsRead`, `rowMutations` and `vmSteps`. These limits may reduce the defaults,
never raise or disable them. Zero is a valid budget. Include them in the exact
saved write body; changing a budget changes its recovery identity.

Client timeout includes connection setup and response reading. Finite server budgets
include queue time. Cooperative deadlines are not process-memory caps. Service
qualification must measure memory and residual uncancellable work before release.
Native work counters bound rows read, mutations and VM steps; negotiated capabilities
must state which statements have checked coverage. No SQL prefix alone proves a
request read-only. Execution details remain engine-enforced.

Client errors have a stable `FDB_*` code and outcome: `not-sent`, `read-failed`,
`unknown`, or `rolled-back`. Only a valid receipt proves the last outcome. Keep
the request journal on unknown errors, plus HTTP status where available. Do not
include tokens, full request bodies or transport errors containing credential
headers in error messages. Authentication/capacity errors remain distinguishable.

## Compatibility and qualification

| Operation | Embedded adapter | Local service | Existing Cloud |
| --- | --- | --- | --- |
| Typed positional results | New client contract | Version 1 | Tagged response version 2 |
| Fresh read-only batch | One managed transaction | `/read`, version 2 | `/read`, version 2 |
| Atomic parameterized batch | One managed worker operation | `/query` | `/query` |
| Durable cross-process replay | Not advertised | Protected in-file receipts | Hosted retained receipts |
| Exact stale-edit condition | Adapter-scoped revision | `ifSequence` | `ifSequence` |
| INFO DB/TABLE/INDEX, EXPLAIN | Checked native APIs | Checked native APIs | Hosted metadata and indexed scalar EXPLAIN qualified |
| Vector/spatial/full-text | Existing checked native features | Same checked frontend | Existing checked runtime; budgets may reject |
| Capability discovery | Adapter reports limits | Negotiated details | Documented contract, no negotiated details |
| Auth/account administration | Not applicable | One protected service token | Outside shared client and Studio release |

Cloud imposes additional scopes, quotas, storage and admission limits. Changing
URL/token does not transfer data or remove those limits. Real Cloud qualification
requires an isolated database, dedicated credentials and recorded public API
evidence. The hosted common SDK fixture and production Studio browser workflow
pass, including typed edits, rejected drafts, rollback and exact uncertain-write
recovery; their dedicated fixtures were cleaned up. Mocked transport tests
establish client behavior only. Vector EXPLAIN remains explicitly unsupported,
and reserved automatic unique-index details remain an engine metadata limitation.

References: [current Cloud API](../../../cloud/docs/api.md),
[database JWTs](../../../cloud/docs/database-jwt.md),
[embedded deployment](deployment.md) and [transfer values](transfer.md).
