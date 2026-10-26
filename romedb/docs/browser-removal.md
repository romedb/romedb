# Native-only V2: browser support removed

The user clarified that V2 needs Rust, Node.js/TypeScript and Python clients, and
requested removing browser support to reduce overhead. TypeScript server apps
running on Node use `@romedb/node`; browser UIs call an application backend API.
No browser database runtime, WASI SDK, OPFS adapter or Firefox/Chromium testing is
required for these native clients. Cloud development remains out of scope.

## Removed from active development

- `romedb/bindings/browser`: Rust reactor/storage, JavaScript workers/client,
  browser npm dependencies, packaging, browser tests and generated output.
- Browser workspace member and frontend threaded-WASI QuickJS bindgen dependency.
- WASM build/check/environment scripts, three WASM examples and the browser probe
  harness under `docs/probes/wasm-browser`.
- Opt-in WASI FTS core exception: isolated revert `ae6777a17` removes the ten-file
  change from `2ef619c07`. The other five approved native core exceptions remain.
- Browser package selection in routine scoped checks and browser release gates.

`romedb-protocol` remains because the Python binding uses it. Native core FTS,
QuickJS user functions, spatial/H3, ANN and RomeQL features remain in scope.
Cargo.lock drops only `romedb-browser` and bindgen 0.72.1 relative to the preceding
tree; no package identity/version/source/checksum was added or upgraded.
Current lock SHA-256:
`eccecb92bd14841e0f0980680eb051f365db9454cbade2212a8ffdd80d3c6443`.

## Retained history

Before removal, source files were archived outside the engine checkout at
`/home/tan/Sites/romedb/archives/browser-experiment-20260925.tar.gz`.
Every archived source file was checked byte-for-byte before deleting the active
copy. The archive excludes generated dist, node_modules, target and cache folders.
Size: 133,468 bytes. SHA-256:
`fe8809d862adaa7ecea66ec576b351ea1376007d132f9d76caf5a0ec333eb7e6`.
This is a local recovery archive, not a release artifact.

Historical proposal patches and milestone evidence remain in docs. The earlier
Firefox shutdown timeout and five later passing targeted repeats are historical
browser evidence, not a native release blocker. Do not restore browser support
or its retired core exception during routine upstream syncs.

## Verification

Offline Cargo metadata confirms the browser package is absent. The native Node
and Python dependency inventories and notices are regenerated for the current
lock: 275/264 declarations and 187/184 collected notice texts respectively, with
four external notice gaps in each. Native platform/artifact qualification and
complete attribution remain release requirements.

The standalone Rust consumer passed both V1 and V2 API fixtures after removal;
341 registry/git identities matched the workspace lockfile. See
`/tmp/romedb-native-only-rust-consumer.log`.

Native-only scoped acceptance completed successfully: 738 Rust tests and 115
Node tests, no failures or ignored/skipped tests, with formatting, Clippy and
TypeScript checks. Log: `/tmp/romedb-native-only-acceptance.log`. The rebuilt
Python wheel passed eight tests on each of CPython 3.10, 3.12 and 3.14.
These checks precede the subsequent PHP/Swift/C#/Go client additions; see
[native language clients](native-language-clients.md) for their separate scope.
