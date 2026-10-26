# Bundled SQLite notice

The RomeDB CLI SQLite adoption command bundles SQLite 3.53.2 through
`rusqlite 0.40.2` / `libsqlite3-sys 0.38.2`. The native database engine and
language client libraries continue to use Turso; this additional SQLite library
is specific to the CLI adoption workflow.

SQLite source identity:
`d6e03d8c777cfa2d35e3b60d8ec3e0187f3e9f99d8e2ee9cac695fd6fcdf1a24`
(2026-06-03 19:12:13).

Pinned archive file: `libsqlite3-sys-0.38.2/sqlite3/sqlite3.c`.
Whole-file SHA-256: `0a409f1633283fa31a9126b11fbfd64a1991c5d30defad07e5745d4667f5e23d`.
The original public-domain notice is retained below. The Rust wrapper's MIT
license is included in the generated CLI crate notice collection.

```text
/*
** 2001 September 15
**
** The author disclaims copyright to this source code.  In place of
** a legal notice, here is a blessing:
**
**    May you do good and not evil.
**    May you find forgiveness for yourself and forgive others.
**    May you share freely, never taking more than you give.
**
```

Source terms: [SQLite public domain dedication](https://sqlite.org/copyright.html).
