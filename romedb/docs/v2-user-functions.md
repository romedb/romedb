# Sandboxed JavaScript scalar functions

This contract is implemented and qualified in the published native Linux x64
RomeDB 2.0.0 release. Browser/WASM is outside this release.

```sql
CREATE FUNCTION app::normalize_name(value string)
RETURNS string LANGUAGE JAVASCRIPT
AS 'return value.trim().toLowerCase();';
SELECT app::normalize_name(name) FROM users;
CREATE OR REPLACE FUNCTION app::normalize_name(value string)
RETURNS string LANGUAGE JAVASCRIPT AS 'return value.trim();';
INFO FOR FUNCTION app::normalize_name;
DROP FUNCTION IF EXISTS app::normalize_name;
```

Definitions persist transactionally, including the exact source, typed signature,
format version and SHA-256 digest. Replacement and removal obey the caller's
transaction. Definition resolution and statement execution share a database
snapshot. Native callbacks receive resolved definitions and values; they never
query the database. There is no JavaScript database handle or recursive RomeQL
invocation. Reserved built-in namespaces cannot be replaced.

Signatures accept `string`, `boolean`, `integer`, `number`, `object`, `array` and
`any`; a `?` suffix admits null. `any` includes null. There are at most 32 named
parameters; argument arity and types, then return types, are checked. Integer
values use JavaScript BigInt across the full signed int64 range; Number values
remain the distinct floating-point type. Use `1n` when returning an integer.
Objects and arrays recursively preserve these types. Input objects have null
prototypes; use `Object.hasOwn` rather than inherited instance methods. Record, binary and vector
arguments require an explicit conversion before the call. Undefined, nonfinite
numbers, out-of-range BigInt, promises, cycles, array holes, accessors, symbols,
non-plain objects and malformed UTF-16 are rejected as results.

Each call creates a fresh QuickJS runtime. Global mutations cannot persist to
another call. There are no host bindings for networking, files, environment,
processes, timers or database access. Clock, random and weak-reference globals
are removed. No pending asynchronous jobs are executed. Successful pure calls
are reproducible for the pinned runtime and inputs; deadlines and memory limits
can still fail a call. Native SQL registration deliberately does not mark the
callback deterministic: callers must not assume invocation counts or eligibility
for persisted SQL expression indexes. UDFs are not supported in CHECKs, defaults,
generated columns, triggers or views.

Current per-call limits: 8 MiB QuickJS heap, 256 KiB QuickJS stack, 100 ms wall
budget and 1,000 interrupt polls; source and encoded argument/result payloads are
bounded to 65,536 bytes, with a 64-level logical value depth. Bounds include
conversion work. They are not process-wide memory or real-time latency guarantees.
Engine cancellation/deadlines are polled inside JavaScript, without database
re-entry. Query result limits remain separate. A failed collection write restores
its entire statement and index changes while preserving earlier explicit
transaction work.

The approved core scalar-read fix also preserves explicit transactions and named
savepoints when a read-only extension callback fails. It applies to bundled and
private frontend helpers as well as user JavaScript. Managed write evaluation and
RETURNING failures still undo that statement's data/index changes; earlier caller
work survives. Native writer handling and other engine errors retain their own
dispositions, so inspect the transaction report before retrying. This differs from
V1's transaction-wide rollback for these scalar read failures.

Rust exposes `create_function` and `drop_function`; RomeQL lifecycle and calls
flow through all seven native clients' execution APIs. `INFO FOR FUNCTION`
returns the source, signature, metadata version, runtime identifier and digest.
Document export remains a document-only format; use migrations or database
backups to transfer function definitions.
