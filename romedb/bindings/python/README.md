# RomeDB for Python

This package embeds the RomeDB frontend. The Linux x64 wheel ships in the
[RomeDB 2.0.0 release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.0.0),
qualified on CPython 3.10.21, 3.12.3 and 3.14.7 on Ubuntu 24.04. Registry
publication is separate. Import `romedb` from the `romedb-embedded` distribution.

```python
from romedb import Database, Record

with Database("app.db") as db:
    db.execute("CREATE TABLE IF NOT EXISTS users")
    user = db.collection("users").insert({"id": Record("users", "alice"), "name": "Alice"})
    print(db.all("SELECT name FROM users WHERE id=$id", {"$id": user["id"]}))
```

Integers are signed int64; floats remain distinct finite binary64 values. Python
`None`, bool, str, bytes, list/tuple, dict, `Record`, and `Vector` map to typed
RomeDB values. Named parameters retain their `$`, `:`, `@`, or `?` prefix. Results
preserve duplicate column positions. `QueryResult` exposes columns, rows, affected,
and transaction observations; `first` returns None for no row and `exactly_one`
requires one row.

Use `transaction()` to hold a connection across a grouped operation, including
nested savepoints. Calls on a connection are serialized and native work releases
the GIL. Explicit SQL BEGIN/COMMIT across calls requires application coordination.
`CancellationToken` supports cancellation from another thread; `timeout_ms` is a
cooperative deadline. Neither guarantees instantaneous interruption of native
work. A token and a timeout cannot be passed together. Closing waits for active
work and is idempotent. A Database context manager closes without auto-committing
an explicit transaction.

The client exposes RomeQL execution, batches with per-statement reports, query
profiling, result/write buffer limits, migrations, JSON/NDJSON document transfer,
collection integrity audits, and basic collection CRUD. Batches are not implicitly
transactional; a BatchError carries completed reports and the failing report.
Document transfer excludes schema, functions and migration history.

Build with the pinned Maturin version from pyproject.toml and the repository's
documented build policy. The 2.0.0 wheel uses the required development profile
with distributed debug symbols stripped; the release includes dependency
notices and installed-wheel qualification. Use one owning process per database
file; independent Python workers cannot open the same live file. This is a
native embedded API, not a DB-API 2.0 adapter, async Python client, or cloud client.
