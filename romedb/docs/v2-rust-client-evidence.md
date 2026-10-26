# V2 standalone Rust consumer qualification

## Native-only refresh (2026-09-25)

After browser removal and the WASI core revert, the standalone consumer passed
again with the approved native scalar-error and FTS/WAL fixes integrated. Both
V1 and V2 fixtures passed; 341 registry/git identities matched the current
workspace lockfile. Log: `/tmp/romedb-native-only-rust-consumer.log`.
Current lock SHA-256:
`eccecb92bd14841e0f0980680eb051f365db9454cbade2212a8ffdd80d3c6443`.
This remains a local Linux development build, not crates.io qualification.
The earlier record below retains its original source and artifact scope.

## Earlier qualification

Working-tree qualification, 2026-09-25, based on core commit `12109384a` and the
active V2 frontend. A private application outside the Turso workspace builds and
runs successfully with only a path dependency on the RomeDB frontend.

`romedb/scripts/check-rust-client.py` creates a separate temporary Cargo workspace,
seeds its lockfile from the repository, and rejects registry/git identity or
checksum drift. Metadata resolution and the executable build run offline. The
script removes RUSTFLAGS, CARGO_ENCODED_RUSTFLAGS and CARGO_BUILD_RUSTFLAGS; the
consumer directory does not inherit this checkout's Cargo configuration.

## Verified behavior

The extended fixture calls these public Rust APIs directly: `create_index`,
`define_relation`, `create_spatial_index`, `create_fulltext_index`,
`create_vector_index`, `search_vectors`, `create_function`, `drop_relation` and
`drop_function`. It uses RomeQL to exercise each search source, record brace
projection with forward fetch, inverse expansion, H3 and a successful stored
JavaScript scalar. Typed record IDs and maximum int64 survive the result paths.

Indexed updates are rolled back before closing; a fresh Database/Connection
reopens the file and sees the original full-text/vector/spatial hits, relation,
projection and function definition. Function/relation removal and FTS teardown
run inside a transaction, physical integrity passes after teardown, and rollback
restores the function and full-text search. Collection integrity also passes.

The existing standalone V1 fixture remains in the same run: typed and portable
values, CHECK/unique validation, transaction observations, all five vector
constructors, profiles/audits, result and write-buffer limits, cancellation and
deadlines, correlated iterator CTEs, write-conflict policies and reopen.
Both fixture success messages are present and the process exits zero.

This does not cover the pending scalar-error transaction failures, qualify browser
FTS, or establish crates.io packaging. The frontend package remains private with
baseline 1.0.0 metadata. The broader V2 release gates stay open.

## Build identity and reproduction

Rust 1.88.0, dev profile with debug assertions, Linux x86_64 / Ubuntu 24.04.1 LTS
under WSL2. C/C++ dependency compilation and linking succeed without caller
Rust flag overrides. The complete compile took 4m 26s; this is a build observation,
not a query-performance benchmark. **346 registry/git package identities** in the
resolved consumer lockfile are a subset of the pinned baseline; this count is
not a linked-component inventory.

```sh
python3 romedb/scripts/check-rust-client.py
```

Log: `/tmp/romedb-v2-rust-consumer.log`.
Script SHA-256: `7478cc0be5410a33df348e2fd1c550701d323c89648b675aa7fd8c8dda49c945`.
Cargo.lock SHA-256: `0dcbf5a9c5ead00c327c86d2a3fca97881ed3de9004e0d30f14a7b6b2f90f5b8`.
Cached test executable:
`target/romedb-rust-consumer/debug/romedb-consumer-smoke`, **301,364,216 bytes**,
SHA-256 `b23ef8fb8b5fe9fa4de82c93d85def3d4fa8e576a36396da66b28c9c6b75aa72`.
The temporary consumer sources, lockfile and database were removed after success;
this cached executable is a test artifact, not a distribution build.
Python syntax compilation and Git whitespace checks also pass.
