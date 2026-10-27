# Python qualification after scalar-error integration

The native Python wheel now includes the approved scalar-read transaction fix
`f26014f04` and all earlier approved native FTS/WAL fixes. This is a local
development artifact, not the final V2 release.

Eight installed tests pass separately in fresh offline environments on CPython
3.10.21, 3.12.3 and 3.14.7. The new regression checks:

- A throwing function inside the Python nested transaction context preserves
  outer work and reports active-to-active; only nested work rolls back.
- A late throwing UPDATE restores all rows and managed index entries.
- A cancelled JavaScript loop preserves caller rows and transaction state.
- The outer context can commit successfully afterward.

All seven earlier tests still pass, including typed persistence, FTS/ANN/H3,
physical integrity after FTS drop, migration/transfer, queue deadlines and GIL
release during cancellation. The prior wheel fails the new test with
active-to-autocommit after the throwing scalar read, reproducing the exact bug.

Artifact:
`/tmp/romedb-v2-python-approved/romedb_embedded-2.0.0.dev1-cp310-abi3-manylinux_2_35_x86_64.whl`
(81,256,302 bytes), SHA-256
`289e10fa3c1289f333246283ab60527dac4ee19c7d795e57e113685480eb468e`.
Built with Maturin 1.12.6, Rust 1.88.0, dev profile, CPython 3.10 stable ABI.
Host remains Ubuntu 24.04 x86_64 / glibc 2.39; the wheel's manylinux tag is not
evidence of execution on every compatible distribution.

Build: `/tmp/romedb-v2-python-approved-build.log`.
Installed logs: `/tmp/romedb-v2-python-approved-310.log`,
`/tmp/romedb-v2-python-approved-312.log`,
`/tmp/romedb-v2-python-approved-314.log`.
Negative control: `/tmp/romedb-v2-python-scalar-control.log`, using the prior
FTS-storage-qualified wheel. No dependency version or cloud change was needed.

Final versioned packages, native platform matrix and complete attribution remain
release requirements. Browser/WASM is outside the user-selected release scope.

## Native-only rebuild

After browser removal, the wheel at
`/tmp/romedb-native-only-python/romedb_embedded-2.0.0.dev1-cp310-abi3-manylinux_2_35_x86_64.whl`
(81,255,560 bytes) passed all eight installed tests on the same three Python
runtimes. SHA-256:
`5282387c31c39f8a932011820bec8d5ef0cc6433fd3803a1936a0ec1128061c2`.
Logs: `/tmp/romedb-native-only-python-310.log`,
`/tmp/romedb-native-only-python-312.log`, `/tmp/romedb-native-only-python-314.log`.
This artifact uses the native-only lock recorded in browser-removal.md and
predates the subsequent shared C ABI workspace member.
