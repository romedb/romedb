# RomeDB 2.3.0 for Linux x64

This bundle contains the embedded RomeDB engine, RomeQL V2 and native clients.
Use one owning process per database file; see DEPLOYMENT.md and OPERATIONS.md.
The supported target is a Linux application server embedding RomeDB; no graph
database API or separate network database daemon is promised.
Verify its checksum file before use. The release record and manifest describe
the exact source, build profile and tested platform. Back up existing databases before opening them with 2.3.0. New managed
features use catalog version 5; older binaries reject this metadata. Restore a
pre-upgrade backup to return to an older release; follow BACKUP.md.

The target baseline is Ubuntu 24.04 x86_64 under WSL2 (glibc 2.39). Install the
distribution's libc6, libgcc-s1 and libstdc++6 packages. The candidate's exact ELF
symbol requirements and installed-runtime results must be recorded in its
qualification evidence before publication. Symbol floors are not a tested
distribution matrix. Alpine/musl and other architectures are outside this bundle.

## Upgrade an existing RomeDB database

Stop the old owning process, preserve a pre-upgrade backup and open the working
copy with 2.3.0. Full-text indexes created by 2.0.0/2.1.0 require an explicit
rebuild before search or writes to their collection:

```sql
REINDEX articles_text;
```

Use each existing full-text index name in place of `articles_text`. Rebuilds are
transactional. Keep the backup until application checks and reopen checks pass.
Version 2.3.0 qualifies upgrades and independent restores from 1.0.0, 2.0.0 and
2.1.0; this does not promise a binary downgrade.

## Install a client

Node.js 22 or newer:

```sh
pnpm add /absolute/path/to/bundle/packages/romedb-node-2.3.0.tgz
```

Python 3.10 or newer:

```sh
python -m pip install /absolute/path/to/bundle/packages/romedb_embedded-2.3.0-cp310-abi3-*.whl
```

The wheel tag expresses its build compatibility check. The release's actual
tested host is recorded separately; it does not imply tests on every Linux
distribution. Node and Python contain their own native engine libraries.

PHP, Swift, C# and Go use the shared native library under `lib/`. Add that
directory to the loader search path before starting the application, for example:

```sh
export LD_LIBRARY_PATH=/absolute/path/to/bundle/lib
```

Extract the corresponding source package from `packages/` and follow its README.
PHP requires FFI and an explicit library path. Swift uses the included SwiftPM
package plus linker `-L`/runtime `rpath`. Go uses a local module replacement,
cgo and `CGO_LDFLAGS=-L/absolute/path/to/bundle/lib`. C# consumes the local NuGet
package and requires the same native library at runtime. Package-registry
publication is not implied by this bundle. Rust uses the included pinned source
archive and a path dependency on `romedb/frontend` with Rust 1.88.0.

## Query documents and spatial data

```javascript
const { Database } = require('@romedb/node');
const db = new Database('places.db');
try {
  db.execute("INSERT INTO places {id:places:cafe,title:'Coffee',p:geo::point(100,13)}");
  db.execute('CREATE SEARCH INDEX nearby ON places(p) USING SPATIAL');
  console.log(db.all("SELECT id FROM search::near('nearby',geo::point(100,13),1000)"));
} finally {
  db.close();
}
```

Use a new database for this example. The Node `AsyncDatabase` API runs database
work on a worker thread and is appropriate for request handlers. Browser UIs
communicate with an application backend. No browser runtime or cloud hosting is
included here.

`bin/romedb-cli` accepts SQL and RomeQL scripts. 2.3.0 adds fixed and bound record sources (`FROM docs:a`, `FROM type::record('docs', $key)`), a shared Node client for file and URL transports, and a local service. The 2.2 language includes document PATCH/CONTENT/MERGE, explicit FETCH/OMIT, schema rules,
compound/array indexes, conflict updates, filtered search, F16 ANN graphs and
statement deadlines. Existing V2 features include indexed spatial/H3,
FTS and ANN search, record brace projections, indexed inverse relationships and
sandboxed JavaScript functions. Read the feature contracts in the source archive
for supported forms and limits. Preserve positional result columns and inspect
transaction reports after an error. Results may materialize in memory;
cancellation is cooperative and configured limits are not a total memory cap.

Native component attribution is under `notices/`, with package-specific copies
where the package embeds a native engine. The source archive includes provenance,
the core exception maintenance register, release evidence and detailed contracts.

## Adopt SQLite files

```sh
bin/romedb-cli sqlite check application.sqlite
bin/romedb-cli sqlite import application.sqlite application.romedb
```

The source is preserved and committed WAL data is included. Unsupported schemas
are rejected before a destination is published. Tables remain relational. Read
SQLITE-ADOPTION.md for the supported-schema boundary and migration procedure.
