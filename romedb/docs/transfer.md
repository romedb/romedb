# Document transfer format v1

The Rust `Connection::export_documents(table, TransferFormat::{Json, Ndjson})` API returns a string. `import_documents(table, input, format)` inserts every document atomically and returns the count. Both target an existing collection. This format transfers data only: it does not include validation definitions, indexes, relational tables, migrations or database recovery state.

CLI usage:

```sh
romedb-cli --export docs source.db > docs.json
romedb-cli --import docs destination.db < docs.json
romedb-cli --export docs --ndjson source.db > docs.ndjson
romedb-cli --import docs --ndjson destination.db < docs.ndjson
```

Create the target collection and its desired validators/indexes before importing. Existing IDs and unique conflicts fail the whole import; there is no implicit overwrite or upsert. Missing IDs use the usual automatic-ID insertion behavior. Present IDs must identify the target collection; imports do not retarget references. A failed import leaves no partial imported data or index entries. Engine errors can abort an enclosing Rust transaction according to the existing transaction contract. The CLI exits nonzero for a transfer operation error and writes a JSON object to stderr with `error.code`, `error.message`, and `transaction.before`/`transaction.after`; stdout stays empty. Successful imports print `{"imported":N,"transaction":{"before":"autocommit","after":"autocommit"}}`. Export stdout contains only the transfer payload. Argument, database-open and stream-I/O failures retain their CLI diagnostics and are not presented as transfer-operation reports. An output failure after a successful import does not undo the committed import.

JSON structure:

```json
{"header":{"format":"romedb.documents","version":1},"documents":[{"type":"Object","value":{"id":{"type":"Record","value":{"table":"docs","key":{"type":"Integer","value":"9223372036854775807"}}},"count":{"type":"Integer","value":"9223372036854775807"}}}]}
```

NDJSON begins with the header object alone on its first line. Each following line is one typed Object entry, with no outer documents array. Exports include a final newline; imports also accept an unterminated final line and CRLF. Blank lines are rejected. Empty transfers use an empty JSON documents array or a header-only NDJSON file.

Values are tagged objects with case-sensitive `type` and, except Null, `value` fields:

| Tag | Value payload |
| --- | --- |
| Null | No payload |
| Boolean | JSON boolean |
| Integer | Canonical signed decimal int64 string, no plus sign or leading zeros |
| Number | String of 16 lowercase hex digits encoding finite IEEE-754 binary64 bits, including signed zero |
| String | JSON string, never interpreted as a reference |
| Binary | Array of byte integers 0–255 |
| Vector | Array of bytes in a validated pinned-engine vector encoding |
| Record | `table` string and `key` tagged Integer or String, with integer keys encoded as decimal strings |
| Object | Map of field names to tagged values |
| Array | Array of tagged values |

The wrapper prevents user objects with `type`/`value` fields from colliding with tags. Unknown header versions, unknown typed fields/tags and invalid values are rejected. Transfer JSON is distinct from the prototype CLI query-result JSON: clients must not assume those result envelopes already use this portable encoding. JavaScript consumers can decode Integer payloads with BigInt without passing them through Number. The native Node client exposes importDocuments/exportDocuments using this same encoding.

Current limits are 64 MiB of encoded input/output and 100,000 documents per operation. Values retain the 64-level logical nesting limit. Tagged JSON adds containers around values, so transfer parsing separately preflights a 136-container wire bound before using the deeper decoder. String contents and escapes do not count as containers; malformed JSON is still rejected. Valid maximum-depth documents can round-trip through both formats. Exceeding either nesting bound returns FDB_LIMIT before import mutations. JSON and NDJSON imports validate documents without retaining the document set, then parse the immutable input again while inserting inside one atomic scope. JSON envelope fields may appear in either order; duplicate, unknown, missing and invalid header fields, and trailing input, are rejected during preflight. The JSON document-count limit is checked during sequence decoding. This trades a second parse for lower intermediate memory use and retains validation before writes. Export reads one engine row at a time and encodes into a bounded output buffer, without collecting all engine rows or decoded documents first. It still returns a complete string; the current row, engine allocations and buffer capacity mean these bounds are not peak-memory guarantees. Export reads share a statement snapshot and order by stored ID; that byte order is not a portable semantic sort order. Input parsing completes before mutations. Duplicate document fields are rejected, including nested objects. Streaming, relational data transfer, total memory accounting, large-transfer benchmarks and full restore rehearsals remain release work.


Rust cancellable transfer methods accept CancellationToken; Node AsyncDatabase transfer methods accept an optional final `{signal}` argument. Pre-cancelled requests perform no transfer work. Active cancellation is cooperative at engine boundaries: interrupted import rolls back its own writes while preserving prior outer work, and export returns a complete payload or an error. Parsing, conversion and serialization have no fixed cancellation latency; completion can win a race. Node errors carry transaction observations. Use the reported outcome to decide whether a retry or explicit rollback is appropriate.


CLI output failures are reported as errors with a nonzero exit, including final stdout flush failures. Migration/import work can commit before writing its success report; an output error does not undo that work. Inspect database state (or rerun an unchanged migration plan to inspect its applied prefix) before retrying writes.


CLI import reads at most 64 MiB plus one sentinel byte, checks the byte limit before decoding text, and reports oversized input as FDB_LIMIT even when the sentinel splits a UTF-8 character. Invalid UTF-8 within the byte budget reports FDB_VALIDATION. Both use the structured transfer diagnostic and occur before import mutations; underlying stream-read errors retain I/O diagnostics.
