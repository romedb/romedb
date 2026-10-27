# Bounded SELECT results

Rust `Connection::select_with_limits(sql, params, ResultLimits)` and
`profile_select_with_limits` now enforce explicit `max_rows` and
`max_payload_bytes` on native and typed SELECT results. Existing APIs retain
unbounded result behavior. Overflow returns `FDB_LIMIT`, discards the result,
and stops collection at the first rejected row. Column metadata is charged even
for empty results. Non-SELECT statements are rejected by these APIs.

One-hop FETCH shares the final payload budget. Primary collection charges row
counts and non-FETCH columns; reference placeholders are excluded. The resolver
counts reference occurrences, then charges each found target with its output
multiplicity before retaining it in the target cache. Repeated reads of the same
stored identity are charged once. Missing targets and null references are charged
after target resolution. All final payload checks precede output cloning. Its independent
16,384-reference and 64 MiB tagged-JSON workspace/output limits still apply.
Temporary references and the deduplicated target cache are not covered by the
final payload budget. Fetch queries retain their atomic snapshot scope.

Payload accounting uses UTF-8 byte lengths for column names, strings, object
keys and record table/string keys; null and booleans cost one byte; numbers and
integer record keys cost eight bytes. Binary and encoded vector bytes count
verbatim. Arrays and objects sum their contents, with no container overhead.
Arithmetic is checked. This bounds retained logical payload, not allocator
usage, the temporary decoded row, engine working memory, or execution time.

Node clients expose `selectWithLimits` and `profileSelectWithLimits` with
required bigint `maxRows` and `maxPayloadBytes`; worker calls accept AbortSignal.
RETURNING and collection write candidates have separate policies described below;
transfer retains its own limits. This does not close the V1 resource gate.

CLI `.select-limit ROWS BYTES SQL`, `.profile-limit ROWS BYTES SQL` and
`.write-limit ROWS BYTES SQL` expose the same policies for one statement. Limits
are nonnegative decimal integers; failures use the existing JSON error and
transaction envelope. See the frontend README for input-mode details.

## Opt-in collection write buffers

Rust connections can opt into frontend collection-write buffer limits:

```rust
let connection = database.connect()?.with_write_buffer_limits(ResultLimits {
    max_rows: 10_000,
    max_payload_bytes: 64 * 1024 * 1024,
});
```

Each candidate or document buffer receives its own row/payload counters. Candidate
SELECT rowsets include column-name bytes, including after bound assignment values
replace preparation placeholders. VALUES buffers count evaluated cell payloads.
Object-patch candidates count patch object keys/values and the target record.
Document snapshot buffers count document keys/values before retaining the next
snapshot. SQL UPDATE checks its completed next snapshot before field validation
and storage mutation; earlier updated rows still rely on the operation savepoint.
Object single-record SQL writes check their snapshot within the operation
savepoint. Overflow returns `FDB_LIMIT`, discards the buffer, and uses existing
write rollback. Candidate queries stop at the first rejected row. Ordinary reads
and explicit returned-result budgets keep their separate policies.

Node clients accept the same policy at open time through `DatabaseOptions.writeBufferLimits`
(`maxRows` and `maxPayloadBytes` are bigint). It does not cap native engine write buffers,
direct single-document Rust methods (`insert`, `upsert`, `patch`, `delete`), parsing,
the temporary row being decoded/evaluated, or total memory across overlapping
buffers. Some snapshot checks occur after mutation and rely on operation rollback.
The setting is opt-in and disabled on ordinary `connect()` connections. Increasing
it by consuming the connection with the builder again allows a retry; this does
not alter transaction state. This is initial buffer coverage, not a completed V1
memory/resource guarantee.

The CLI accepts `--write-buffer-limits ROWS BYTES` for SQL input modes and
`--migrate`. Both values are nonnegative decimal integers fitting the platform's
`usize`; the policy is fixed for the process connection and disabled by default.
Transfer and integrity-audit modes reject this option because their separate
policies apply. For example:

```sh
romedb-cli --write-buffer-limits 10000 67108864 --script app.db < changes.sql
```

The policy also applies to collection writes executed by batches and migrations.
A batch reports `FDB_LIMIT` for the rejected statement and stops before later
statements; earlier successful statements keep their normal transaction state.
The migration runner wraps a rejected statement as `FDB_MIGRATION` with an
underlying `FDB_LIMIT` source and rolls back every pending script and history row.
Previously applied migrations remain intact. Rust callers can increase the policy
and retry the identical migration plan; Node callers must open a connection with
the larger policy.

## Atomic write-result policy

Rust `Connection::write_with_result_limits(sql, params, ResultLimits)` accepts
one SQL INSERT/UPDATE/DELETE or supported object write. It runs inside an
operation savepoint and releases only on success. Native SQL checks metadata
before execution and each decoded row before frontend retention. Collection SQL and object RETURNING projections check each evaluated row
before retaining it, including document-star output. Native destinations with
logical sources also pass the budget to their final lowered statement collector.
Internal lowering queries do not inherit that budget. All routes share the same
payload accounting. A result limit failure rolls
back that operation, including data and managed indexes, while preserving prior
pending work when savepoint recovery succeeds. Existing engine/rollback errors
retain their own error handling and transaction disposition.

This is a result acceptance policy with progressive native frontend collection,
**not a bound on engine RETURNING buffers, write candidates, document snapshots
or individual projection working memory**. It must not be used as a process memory cap.
Transaction control, schema operations, reads and multi-statement input are
rejected. Writes without RETURNING can succeed with zero budgets; affected rows
are not returned rows. Empty RETURNING results still charge column metadata.
`write_with_result_limits_cancellable` applies the existing cooperative token
contract: pre-cancelled tokens reject before execution, and delivered engine
cancellation uses savepoint recovery. Source and RETURNING cancellation with
prior-work preservation and fresh-token retry have initial Rust coverage.
Completed-result accounting has no fixed cancellation latency. Both Node
clients expose `writeWithResultLimits(sql, limits, parameters?)`; workers accept
a fourth `{ signal }` argument. Broader cancellation/I/O qualification and
broader candidate/snapshot and engine working-memory qualification remain unfinished.

The original integration inventory follows; its next-step language describes
the design preceding the initial Rust implementation.

## Result budgets — implementation design

Status of the inventory below: design notes retained for remaining integration work; the Rust SELECT subset above is implemented.

## Current execution paths

| Path | Current owner | Accounting concern |
|---|---|---|
| Ordinary SQL results and profiling | `Connection::native_profiled` in `frontend/src/lib.rs` | Collects engine rows, then constructs public values; transient and public representations can overlap. |
| Collection SELECT and profiling | `Connection::execute_lowered_profiled` in `frontend/src/select.rs` | Decodes typed values per row, then appends to a public result vector. |
| Forward FETCH | SELECT collector plus `frontend/src/links.rs` | The outer result survives while target records are resolved. Existing 16,384-reference and 64 MiB resolver limits do not bound the complete result. |
| INSERT SELECT | `Connection::insert_select` in `frontend/src/select.rs` | Source candidates are materialized before writes. They are statement workspace, not simply returned rows. |
| Collection writes and RETURNING | `frontend/src/write.rs`, `update.rs`, and object statements in `lib.rs` | A result limit reached after mutation must not leave a partial statement reported as a harmless read failure. |
| Internal catalog/validation reads | `Connection::run` and shared `collect_rows` | User result limits must not silently truncate these rows or change constraint decisions. |
| Transfers | `frontend/src/transfer.rs` | Existing incremental encoding and transfer bounds are separate from general SELECT result budgeting. |

The common `collect_rows` function is therefore not a sufficient single enforcement point. Applying a global row cap there would affect internal database operations while missing typed SELECT collection and FETCH expansion.

## First implementation step

Introduce an explicit bounded SELECT operation using the existing SELECT-only preparation checks in `profile_select_inner`. Share one collector budget between native and logical SELECT execution, and expose the same policy to profiling. Reject non-SELECT statements before running them. Keep existing APIs and their current behavior until the broader policy is decided and implemented.

Define separate maximum returned-row and logical-payload-byte counters. Count column names and recursively count owned value payloads, including object keys, record table/key strings, arrays, binary and vector data. Use checked arithmetic. Document that these counters bound returned logical data, not Rust allocator overhead or the engine's sort/group/CTE working memory. Check the next row before retaining it, and discard the result on overflow with `FDB_LIMIT`; never return a truncated successful rowset.

Forward FETCH requires the same final-result budget before retaining resolved values. A read-only first step can reject FETCH until this accounting is integrated, but that restriction is unfinished work rather than completion of the V1 link/resource contract.

## Required verification

- Native and collection results: zero limits, exact row/byte boundaries, one-over boundaries, empty rows and empty result metadata.
- Exact payload accounting for multibyte text, nested objects/arrays, records, binary and each vector encoding; overflow-safe arithmetic.
- A volatile callback confirms collection stops at the first rejected row and no later row executes. Planning executes no callbacks.
- Error provenance remains `FDB_LIMIT`; engine `Interrupt`, `Busy` and transaction observations retain their own meanings.
- Rejected read results preserve prior pending writes and allow a valid query retry. Non-SELECT input performs no mutation.
- Sync and worker clients use the same limits, with cancellation and lifecycle checks, strict TypeScript and installed-package coverage.
- FETCH, write candidates and RETURNING receive explicit integration and atomicity tests before claiming general query budgeting.

## Remaining V1 resource work

Collection RETURNING now consumes document snapshots sequentially, releasing each
completed snapshot as output accumulates and avoiding a separate vector of
borrowed snapshot references. SQL UPDATE also consumes the owned candidate document
and evaluated assignment values instead of cloning them immediately before
mutation. SQL DELETE likewise moves its candidate document into the RETURNING
snapshot buffer and uses it for storage cleanup, avoiding a second document read
and decode. Its snapshot check precedes storage mutation. Candidate formation
still completes before any writes. Document validation and storage encoding now
borrow document fields instead of cloning the document into a temporary object
value. Encoding writes directly into the prefixed storage byte buffer. INSERT
returns its owned document, and PATCH consumes its owned patch fields. These
changes avoid redundant copies without changing the stored value format. Unprocessed
snapshots and complete write candidates can still be materialized; these lifetime
and ownership changes are not a total-memory cap.


Returned-result limits are only one part of the gate. Engine working memory, statement candidate buffers, deadlines, interrupted I/O and commit/checkpoint outcomes, transfer peak memory, and platform qualification remain required. A green bounded-SELECT test cannot establish a total-memory guarantee.

## Cooperative statement deadlines

Rust cancellable operations accept `CancellationToken::with_deadline(Instant)`.
Async Node operations accept `ExecuteOptions.timeoutMs`; the native deadline starts
before queue submission, after local parameter encoding, and includes queue time.
The CLI exposes `.timeout MILLISECONDS SQL` for one statement in line/interactive
mode or as the entire script input. Interactive SQL can span lines until complete.
Node and CLI milliseconds are nonnegative uint32 values; zero is already expired.

Expiry uses existing cancellation handling and `FDB_CANCELLED`, preserving each
operation's transaction contract. Deadlines are cooperative: engine progress checks
observe them, but parsing, non-engine evaluation, cleanup and response transport
have no fixed time bound. Completion may win a race. The CLI command does not set
a persistent connection policy or combine with other dot-command wrappers.
