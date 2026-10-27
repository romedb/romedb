# RomeDB embedded database

RomeDB and RomeQL [2.9.1 are released for Linux x64](docs/release-2.9.1.md),
with native Rust, Node.js/TypeScript, Python, PHP, Swift, C# and Go clients.
See the [completed checklist](docs/v2.9.1-tasks.md),
[verified download](docs/release-2.9.1-download.json),
[language contracts](docs/v2.2-language.md) and [engine provenance](UPSTREAM.md).

2.9.1 moves the engine to Turso 0.8.3: MVCC durability fixes, AUTOINCREMENT no
longer reuses an explicit id 1, and MVCC journal-mode switches inside a transaction
are refused. Stored data is unchanged.
2.9.0 lets the Node SDK and CLI use Cloud query version 3: Binary and Vector values
travel as base64 and requests may be up to 1 MiB. It also keeps keyword column names
unquoted when a subquery reads a document table.
2.8.0 joins collections with relational tables that store record IDs as text
(`ON u.id = p.user_id` with `posts.user_id = 'users:tan'`) and resolves
unqualified names in collection joins; see the
[contract](docs/contracts.md#joining-collections-with-relational-tables).
2.7.0 reports the engine release: `SELECT romedb_version()`, `romedb --version`
and `engineVersion` in the database details. 2.6.0 made Cloud connections start
faster: `connect()` skips the details
request and warms the database in the background. 2.5.4 added `romedb://`
connection URLs. 2.5.3 made
collection scans and grouping cheaper per document. 2.5.2 made
spatial `EXPLAIN` read no data. 2.5.1 made field reads and filtered
nearest search faster and metered nearest planning work. 2.5.0 added a bounded spatial model: Polygon and MultiPolygon values, exact
point-in-region predicates, `ST_*` aliases, more H3 functions, a two-dimensional
spatial index and region indexes for stored zones. See the
[spatial contract](docs/spatial-upgrade.md). Spatial indexes created or rebuilt
by 2.5.0 move their collection to catalog version 7, which 2.4.0 and older refuse
to open; existing latitude-format indexes keep working and `REINDEX` upgrades them.
2.4.0 added a property graph: edge tables, `RELATE` and bounded `GRAPH(...)`
traversal ([graph contract](docs/graph.md)). Preserve a pre-upgrade backup; legacy
full-text indexes require explicit REINDEX. See the
[quickstart](docs/v2-release-quickstart.md), [deployment contract](docs/deployment.md),
[operations guide](docs/operations.md) and [SQLite adoption](docs/sqlite-adoption.md).
The [operating envelope](docs/production-envelope.md) retains dated 2.1.0
measurements. Cloud hosting uses a separate private deployment; browser/WASM,
graph algorithms and general GIS are excluded.
The 2.2.0 language contracts, [2.3.0 record sources](docs/record-sources.md),
[2.4.0 graphs](docs/graph.md) and [2.5.0 spatial](docs/spatial-upgrade.md)
supersede older subset descriptions below.

From the repository root:

```sh
romedb/scripts/check.sh
cargo run --locked -p romedb-cli -- /tmp/example-romedb.db < romedb/examples/persistent.romeql
```

Use a fresh database for the example. `romedb/examples/sql-writes.romeql` additionally exercises column-list inserts, nested SET, and transaction rollback. The CLI opens an interactive prompt for terminal input and reads semicolon-delimited scripts from piped stdin. It supports multiline statements and emits tagged JSON results with statement byte offsets. Use `--interactive` to force prompts or `--script` to force EOF-delimited script input. SQL input buffers default to 16 MiB; `--max-input-bytes N` changes the byte limit. Each report is written and flushed before the next statement executes. It stops at the first execution or output error and exits nonzero; an output failure does not undo prior committed work. Use `--line` for legacy one-statement-per-line processing that continues after errors; this mode also exits nonzero if any statement fails. It is not the final import/export or client wire format. `romedb --version` prints the RomeDB release; `SELECT romedb_version()` returns it from SQL.

On Unix terminals, the CLI supports cursor editing and up/down history recall, including multiline statements. Ctrl-C at the prompt clears pending input and leaves any transaction active; `.quit` exits and connection close rolls back uncommitted work. History stays in memory by default. Use `--history /path/to/romedb-history` to load and save it across sessions; start a statement with a space to omit it. History retains up to 100 entries and skips new entries over 64 KiB. The submission byte limit is checked after terminal line editing and does not bound the editor's live buffer. Ctrl-C during engine execution requests cancellation and returns an error report before the next prompt. Inspect the report's transaction state; interrupted work can end an outer transaction. See the release record for supported platforms and documented limits.

The Rust client entry point is `romedb::Database::open(path)?.connect()?`. `execute(sql, &Parameters)` handles the implemented RomeQL subset and ordinary SQL. Typed `insert`, `get`, `patch`, `delete`, `define_field`, `create_index`, and `lookup_index` APIs exercise document storage without exposing raw engine access. Collection SELECT supports the initial AST-lowered subset in status.md. SQL-shaped VALUES inserts, SET/UNSET/DELETE, and ID-based UPSERT use the document validation path. Collection/index drops and field removal are transactional; INFO reports logical schema metadata. Field CHECK expressions validate final candidates and existing data when definitions change. V1 catalog version 2 remains readable; spatial indexes and [inverse relationship declarations](docs/v2-relations.md) require version 3; new/rebuilt full-text indexes require version 4. See the [spatial contract](docs/v2-spatial.md) for upgrade behavior. SQL SELECT/SET/VALUES now support typed document/array/record helpers and lazy null helpers; see status.md for the supported subset. See the V2 checklist for future language and engine work.

V2 [full-text search](docs/v2-fulltext.md) provides managed multi-field text indexes
and `search::text(index,query,limit)` with typed IDs and BM25 scores. Read its
ranking, post-filter and memory contracts before using it.

The [INSERT conflict guide](docs/insert-conflicts.md) covers insertion recovery and replacement versus object UPSERT. The [UPDATE conflict guide](docs/update-conflicts.md) compares ABORT, ROLLBACK, FAIL, IGNORE and REPLACE, including retained changes on errors and transaction recovery.

Use `connection.execute_report(sql, &params)` when error handling needs transaction observations. It returns the original result/error and before/after `TransactionState` values. `transaction_state()` also reads the current state directly. CLI JSON lines include `transaction.before` and `transaction.after`; runtime engine errors may end an outer transaction. See contracts.md for observation limits.

`connection.execute_batch(script)` returns per-statement execution reports with UTF-8 byte offsets and stops after the first failure. It does not wrap the script in a transaction: include BEGIN/COMMIT when needed and inspect transaction state after errors. Lexical splitting errors are detected before any statement runs. The final statement may omit its semicolon. Batch scripts currently use literal values; use `execute`/`execute_report` for bound parameters.

A tested [offline backup and restore procedure](docs/backup-restore.md) preserves relational data, collection schema/indexes and migration history using an exclusive maintenance window and successful checkpoint. Online backup and interrupted-copy/recovery qualification remain pending.

`INFO FOR DB` lists tables and views. `INFO FOR TABLE name` reports logical collection validation/indexes or native relational column and index metadata. `INFO FOR INDEX name` includes explicit relational index SQL and key details such as expressions, direction and collation. See [inspection contracts](docs/contracts.md#relational-inspection-details) for the prototype result shape.

The [native Node client](bindings/node/README.md#local-package-smoke) includes an offline tarball-install smoke for local packaging qualification. Linux x64 binaries are included in the V2 release bundle.

The [Rust client guide](docs/rust-client.md) describes local path dependencies and the standalone offline consumer smoke. Registry distribution remains pending.

The [local benchmark harness](docs/benchmarks.md) measures document filters and exact-vector queries through the CLI, with result checks, query plans and Linux process peak RSS. See the release record and benchmark evidence for measured workloads and limits. Use `.profile SELECT ...` in the CLI or `Connection::profile_select` in Rust to obtain primary engine counters alongside query results.

Audit a stored collection with `romedb-cli --check-collection posts app.db`. Optional `--max-documents N` and `--max-encoded-bytes N` override the defaults of 100,000 documents and 64 MiB. The command prints one JSON report and exits nonzero on failure. See the [audit contract](docs/contracts.md#explicit-collection-content-audit) for coverage and resource limits.


### CLI result limits

Use an explicit command followed by nonnegative decimal row and payload-byte
budgets and one complete SQL statement:

```text
.select-limit 100 65536 SELECT title FROM posts;
.profile-limit 100 65536 SELECT title FROM posts;
.write-limit 100 65536 UPDATE posts SET published=true RETURNING id;
```

These commands work as standalone EOF-delimited input, or as individual commands
in line/interactive mode. In interactive mode, the SQL following the command and budgets can span
multiple lines; finish it with a semicolon. `.clear` discards the pending command.
Piped standalone input can also contain multiline SQL. They use the same
result accounting as the Rust/Node APIs: column names count even for empty results,
FETCH output counts after expansion, and overflow emits `FDB_LIMIT` without partial
rows or profile counters. Write-result rejection rolls back the operation through
its savepoint, with transaction observations in the error report. `.write-limit`
accepts data writes; transaction control and DDL use ordinary SQL commands.

These budgets do not cap candidate/snapshot buffers, engine working memory,
individual decoding allocations or execution time. See
[result-budgets.md](docs/result-budgets.md) for exact accounting and limitations.
Existing `.profile`, ordinary scripts, input byte limits and exit-status behavior
retain their contracts; line and interactive modes continue after a failed command
and exit nonzero if any command failed.

V2 development also includes [dense ANN search](docs/v2-ann.md), qualified under
its documented native contract. Release `2.6.0` uses optimized
`romedb-production`; the [release checklist](docs/v2.6.0-tasks.md) is complete.
See the [verified public download](docs/release-2.6.0-download.json).

The [Python binding](bindings/python/README.md) embeds the same frontend at
version `2.6.0`. V2 release clients are Rust, Node.js/TypeScript, Python, PHP, Swift, C# and Go; browser/WASM is
removed from active development and excluded from this release. Current Linux
installed-wheel checks are recorded in the [2.6.0 evidence](docs/release-2.6.0.md).

[PHP, Swift, C# and Go native clients](docs/native-language-clients.md) share a RomeDB C ABI. Local builds and the common contract test are documented there.
