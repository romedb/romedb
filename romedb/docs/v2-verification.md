# V2 spatial foundation verification — 2026-09-25

Working tree based on `03a67fc02`, including the pre-existing uncommitted engine
WAL integration. These results do not identify a published artifact or V2 release.
Toolchain: Rust 1.88.0; Node 24.19.0; TypeScript via pnpm.

## Combined milestone check

- `cargo test --locked -p romeql-parser -p romedb -p romedb-cli -p romedb-tests`:
  686 passed, zero failures and zero ignored. Log: `/tmp/romedb-v2-rust.log`.
- Rebuilt `romedb-node`; Node client/application suite: 107 passed, zero failures.
  Log: `/tmp/romedb-v2-node.log`.
- Five RomeDB packages passed formatting and all-target Clippy with warnings denied.
  Log: `/tmp/romedb-v2-clippy.log`.
- `pnpm --dir romedb/bindings/node typecheck`: passed.
  Log: `/tmp/romedb-v2-typescript.log`.

## Final precision correction

After the combined build, review found that adding 180 degrees before longitude
wrapping could erase extremely small separations. Wrapping now occurs only when
needed. The final regression proves a 1e-14-degree separation remains positive
and symmetric. The combined counts above precede this correction; they are not
an additional full-suite run on the final source.

Final source passed:

- Two spatial unit regressions: `/tmp/romedb-v2-spatial-final.log`.
- Three real-engine spatial integration tests:
  `/tmp/romedb-v2-spatial-integration-final.log`.
- Rebuilt Node addon and the spatial test exercising both sync/async clients:
  `/tmp/romedb-v2-node-final.log`.
- Formatting and all-target frontend/test-package Clippy with warnings denied:
  `/tmp/romedb-v2-clippy-final.log`.

 Tests cover the scalar contract,
not indexed spatial search, cell aggregation or ellipsoidal geodesy. No upstream
core or cloud source was changed for this milestone. Existing WAL changes were
preserved. Local logs are supplemental; committed regression tests are durable
and can be rerun from the working tree.
