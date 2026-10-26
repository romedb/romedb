# Native production build and workload qualification

This policy applies to new production candidates after 2.0.0. It does not change
the historical 2.0.0 artifacts, which used the development profile.

## Shipping build

Rust uses `cargo build --locked --profile romedb-production`. This named profile
inherits `dev`, uses optimization level 3 and thin LTO, and explicitly retains
debug assertions, overflow checks and unwinding. It has 16 code generation units,
line-table debug information and no incremental compilation. Repository policy
continues to prohibit the Rust `--release` flag. C# packaging uses its normal
`Release` configuration; that is independent of Rust's profile.

`build-v2.py` and Maturin both select this same profile. The builder pins profile
environment overrides, rejects ambient Rust compiler flags and checks Cargo's
reported optimization/assertion/overflow fields for all four native entrypoints.
The manifest records the policy, Cargo artifact receipts, build commands, source
Cargo/config hashes and toolchain. Distributed native copies are stripped while
Cargo originals remain available for diagnosis. `check-v2-bundle.py` rejects a
development-profile bundle and verifies the archived source profile before
qualifying the exact binaries. Source archives include the workspace profile.

A downstream Rust workspace owns its profiles. The shipped source's profile is
not inherited automatically by a path-dependent application. The standalone
consumer check copies this policy into its own workspace when invoked with:

```sh
python3 romedb/scripts/check-rust-client.py --shipping-profile
```

Changing optimization policy requires rebuilding and requalifying the artifacts;
stripping a development binary alone does not qualify it as optimized.

## Previous-version upgrade checks

`scripts/upgrade_matrix.py` lists every upgrade/restore check and the first
candidate version that runs it; a check applies to all later 2.x candidates, so a
new release adds only one row for its direct predecessor. The bundle check takes
the published Node packages of earlier releases as repeated
`--previous-package VERSION=PATH` options and fails if the table needs one that is
missing. The Node scripts accept candidate versions by range
(`scripts/versions.cjs`), not by list.

Run the same table before tagging, against the development package
(`romedb/bindings/node` with its built addon) or an extracted release package:

```sh
python3 romedb/scripts/check-upgrades.py /absolute/new/evidence/dir \
  --package romedb/bindings/node \
  --node /path/to/node22 --node /path/to/node24 \
  --previous-package 1.0.0=/path/to/node-1.0.0 --previous-package 2.0.0=/path/to/node-2.0.0 ...
```

A failure there means a script or the table needs a change; fixing it before the
build commit is tagged avoids rebuilding and requalifying the bundle.

## Bounded exact-artifact workload

```sh
node romedb/scripts/bench-production.cjs \
  --bundle /absolute/path/to/candidate \
  --output /absolute/path/to/application-benchmark.json \
  --rows 1000 --dimensions 32 --operations 5000 --connections 1,4 \
  --concurrent-rounds 1000 --samples 1
```

The harness verifies bundle checksums, installs its Node package offline with
pnpm, compares the installed addon to the package, and measures each connection
count in a separate process. It never loads the checkout's addon. Production
evidence requires a committed source candidate and the production profile.
`--rehearsal` permits testing the harness against historical development-profile
bundles and marks the output as non-production evidence.

The generated document corpus has roughly 256 bytes of text per document, one
float32 vector and one geographic point, with scalar, FTS, ANN and spatial checks.
The 40/20/20/10/10 operation mix measures document reads, indexed updates, FTS,
ANN and spatial lookups. Half the FTS searches match the whole corpus. Writes
change indexed text and vectors. Every result is checked against generated data,
including returned ANN distances and exact-neighbor recall. The workload records:

- Separate corpus and index build times; connection-open and first-query times.
- Warm p50/p95/max latency per operation and whole-loop operations/second.
- RSS samples, process high-water RSS, database and WAL sizes, checkpoint cost.
- Per-connection count, query plans proving index paths, post-write integrity,
  reopen verification, VM deadline cancellation and a successful retry.
- Exact package/addon/manifest/checksum/harness hashes and host/runtime identity.

The original mixed phase executes connections serially within one owning
process; its throughput does not imply parallel execution. A separate concurrent
phase uses `AsyncDatabase` worker threads inside that same process: one writer
and the remaining connections as readers. Each bounded round starts a write
transaction and all reader transactions together, then waits for completion
before the next round. This bounds pending requests and keeps readers active
throughout writing. With one connection, this phase is explicitly skipped.

The writer changes indexed text and vectors. Each reader pins a transaction
snapshot, reads the writer's target document, then verifies FTS, ANN or spatial
results against that exact observed document revision. Either committed revision
is allowed; a mixed document/index snapshot is a failure. Reports include
transaction throughput, per-lane p50/p95/max latency, Busy and BusySnapshot
counts, rollback/restart and commit retry counts, backoff, resources, final
integrity and checkpoint costs. Final reopen verification includes every
acknowledged concurrent write.

Only explicit Busy errors have bounded retries. A failed BEGIN must remain in
autocommit, or an active transaction must successfully roll back before its body
can restart. Busy during COMMIT retries that same COMMIT only while the existing
transaction remains active. Ambiguous commit errors and other failures stop the
run without replaying writes. A separate deterministic contention probe holds
the writer lock, verifies a competing writer receives Busy, then verifies retry
after confirmed lock release; its counts are separate from natural contention.
Concurrent transaction latency includes assertions and retry/backoff costs.

A cold connection does not imply an evicted OS cache. Peak RSS
includes setup and the Node runtime. The generated vectors are a deterministic
correctness workload, not a general embedding-recall benchmark. Cancellation
does not claim to preempt native ANN/FTS calls. Automatic WAL checkpoints remain
enabled above 1000 unbackfilled WAL frames with `synchronous=FULL`; the pinned
engine does not implement `PRAGMA wal_autocheckpoint` overrides. An explicit truncate checkpoint is
measured and its success checked before closing.

## Finite qualification plan

- [x] Encode the shipping profile and artifact provenance policy.
- [x] Add a reproducible exact-package workload and reject mislabeled evidence.
- [x] Build and verify a clean production candidate with all seven clients.
- [x] Measure 1,000 documents / 32 dimensions and 5,000 documents / 128 dimensions,
  each at one and four connections, 5,000 mixed operations and 1,000 concurrent
  rounds where multiple connections are configured, on the supported host.
- [x] Select and publish a supported envelope from those measurements, including
  FTS corpus/match limits, ANN dimensions, connection count, observed resource
  peaks and explicit operating headroom; reduce scope or fix code if it fails.

Both configurations passed on the exact 2.1.0 artifacts. The
[measured envelope](production-envelope.md) records results and supported
scope; this is not a universal latency, memory or scale guarantee. The harness rejects more than 10,000 documents, 256 dimensions, eight
connections, 50,000 mixed operations, 10,000 concurrent rounds or three samples per connection count to keep
maintainer experiments finite; those caps are not a production support promise.
