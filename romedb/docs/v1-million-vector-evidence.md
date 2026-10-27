# S7 million-vector capacity evidence

The bounded 1,000,000-document, 768-dimensional seeded float32 evaluation
completed successfully on 2026-09-14, exit status 0. The unchanged
[raw report](benchmark-results/2026-09-14-linux-release-seeded-768-1000000.json)
contains three samples per workload, plans, engine counters and the independent
cosine reference. Together with the retained 100k release reports in
[benchmarks.md](benchmarks.md), this supplies the requested 100k–1m evaluation
points. It does not promise interactive performance at that size.

| Workload | Median seconds | Maximum / three-sample p95 seconds | Engine rows read per sample |
|---|---:|---:|---:|
| Unindexed scalar filter | 112.856 | 115.989 | 1,000,000 |
| Indexed scalar filter | 1.507 | 1.512 | 20,000 |
| Exact cosine top 10 | 405.927 | 410.516 | 1,000,000 |

The indexed predicate matches 10,000 documents; its 20,000 engine reads include
index and document rows. It performs zero fullscan steps, and the plan names
docs_group plus the backing document primary-key lookup. Exact vector search
performs 999,999 fullscan steps and one sort, with no vector index.

Peak CLI process RSS was 26,047,909,888 bytes (about 24.3 GiB); checkpointed
database size was 12,482,203,648 bytes (about 11.6 GiB). RSS is a process lifetime
high-water mark including loading and earlier queries, not isolated vector
working memory. Load took 2,278.883 seconds and index construction 154.901 seconds.
Compilation/tests for independent work overlapped loading, so load throughput
is not an uncontended measurement. No heavy build/test run overlapped measured
query phases; brief focused Node acceptance tests did run during the vector
phase. This is a development-machine measurement, not an isolated benchmark host.

## Provenance and validation

Binary source was clean commit `9a82a36ea`, built with Rust 1.88.0 using
`cargo build --locked --release -p romedb-cli` (optimized profile with debug
information). Binary SHA-256 was verified after completion:
`c3790cdfc097d6e82018f51569fe694202d5994b9e1fd85d1f5f876ca1dafa24`.
The report's commit field is `6b9076f2562c5adc040ac439691d413c9a89090f` because
the harness observes Git at report creation. Intervening changes were docs and
tests, including a cfg(test)-only recovery module; the release binary was not
rebuilt. The raw report is preserved without rewriting that observed field.

Command: `timeout 7200 python3 romedb/scripts/benchmark.py --binary target/release/romedb-cli --rows 1000000 --dimensions 768 --samples 3 --fixture seeded --output /tmp/romedb-v1-million-768.json`.
Host was Linux x64 under WSL2, glibc 2.39, Python 3.12.3. Each workload had one
warmup followed by three timed CLI round trips including JSON transport.

The harness checks scalar counts and index plan use. For every vector result it
checks ten distinct in-range keys, sorted distances with key tie breaking,
membership relative to an independent float64-fsum reference over rounded
float32 coordinates, and distance error at most 2e-6. These checks completed
before the report was emitted; no failed or partial sample is presented as a pass.

## Release disposition

S7's missing capacity measurement is complete. At this measured scale, exact
vector search takes roughly 6.8 minutes and the process reaches substantial
memory usage. Applications needing low-latency million-vector retrieval exceed
the demonstrated V1 capability. ANN remains V2; this result does not silently
introduce an ANN requirement into V1. Retain these practical limits in release
notes. Reuse this evidence unless a candidate changes the measured execution
path; do not repeat the same large run solely for docs or test changes.
