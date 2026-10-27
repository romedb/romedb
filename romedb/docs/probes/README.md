# ANN native probe

This standalone probe uses USearch 2.26.2 without default features. It measures
native HNSW against native exhaustive search and tests buffer restore, deletion,
replacement and independent graph instances. It does not measure RomeDB.

Copy ann-native.rs to a temporary crate's src/main.rs, ann-native.Cargo.toml to
Cargo.toml and ann-native.Cargo.lock to Cargo.lock. Run with Rust 1.88.0:

```sh
ANN_POINTS=100000 cargo run --locked --manifest-path /path/to/probe/Cargo.toml --bin romedb-ann-probe
```

The crate uses a fixed generator seed, dimensions 64, 64 held-out queries,
connectivity 32, construction expansion 200 and search expansion 512. Scalar
float32 kernels are used. Results depend on machine/load and are evidence for
this fixture, not a guarantee for other embedding distributions. The Rust binary
uses a debug build; the dependency's own build script compiles C++ with O3 and
fast-math on Linux. Do not describe it as a fully optimized RomeDB release build.

For the actual frontend, run `cargo run --locked -p romedb --example ann_benchmark`
from the repository. That separate fixture compares public RomeQL ANN queries
with exact SQL cosine search over 10,000 vectors and checks returned distances.

`ann-topology.rs` is a small control/fix experiment. Run it as a separate binary
in the same temporary crate. It reproduces the problematic per-insertion reserve
sequence (256 nodes, no upper-layer nodes) and compares one upfront reservation
(256 nodes, 10 upper-layer nodes). The production adapter uses upfront/geometric
capacity and has a regression checking that an upper layer is a proper subset.

## Removed WASM probes

Browser/WASM probe sources and build scripts were removed at the user's request.
Their source archive and historical evidence are recorded in
[browser removal](../browser-removal.md). Do not restore them for native release
qualification. `fts-wasm-experiment.patch` is retained as historical review data.
