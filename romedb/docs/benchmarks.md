# Benchmark harness and retained release evidence

Current performance claims use [the production envelope](production-envelope.md).
The observations below are historical V1 exact-search evidence, not current ANN
performance. Intermediate development runs were removed during [cleanup](documentation-cleanup-20261001.md).

## Running new measurements

Use `romedb/scripts/benchmark.py` with the intended qualified CLI and record its
binary/source identity, workload, dimensions, samples, and limits. Run heavy builds
and benchmarks through `/home/tan/Sites/fastdb/scripts/romedb-heavy`; do not run
heavy workloads concurrently. Keep temporary output in the workspace `.local/logs/`
and remove it after retaining a concise result. Historical commands below record
previous runs, not current build instructions.

## Optimized 100,000 × 768 comparison (2026-09-07)

The [release-profile report](benchmark-results/2026-09-07-linux-release-seeded-768-100000.json) completed successfully at clean commit `83583ecdda91aa3f595dcef7d39545523520ac47`. Build command: `cargo build --locked --release -p romedb-cli`, with Rust 1.88.0 and the same dependency lockfile. The existing release profile uses thin LTO, four code-generation units, abort-on-panic and line-table debug information; compilation took 2m48s. This is the `release` profile, not `release-official` or a published artifact.

Run command: `python3 romedb/scripts/benchmark.py --binary target/release/romedb-cli --rows 100000 --dimensions 768 --samples 3 --fixture seeded --output romedb/docs/benchmark-results/2026-09-07-linux-release-seeded-768-100000.json`. The unchanged harness hash is recorded in the preceding diagnostic. The binary SHA-256 was verified against the report: `80d383d176fb1729041b5da09fd63601a5eebd132d27b746f2e1d0d676e54193`.

| Workload | Optimized median | Optimized sample range | Debug median |
|---|---:|---:|---:|
| Unindexed filter | 9.48 s | 9.33–9.52 s | 82.45 s |
| Indexed filter | 138.64 ms | 132.74–148.26 ms | 1,124.82 ms |
| Exact cosine top-10 | 47.12 s | 47.03–47.29 s | 388.11 s |

Loading took 190.51 seconds and index construction 13.03 seconds. The checkpointed size was identical at 1,247,805,440 bytes. All filter and vector reference assertions passed for warmups and measured samples. Independent reference values, per-workload primary engine counters and database size match the debug report exactly; medians and current binary identity were independently checked. The intervening commit contains only diagnostic documentation and its report, so product code and harness were unchanged.

Reported VmHWM observations are 2,622,300,160 then 2,620,866,560 bytes (about 2.44 GiB). The decreasing accounting caveat from the debug run recurs; these observations do not establish a reliable monotonic peak or a meaningful memory improvement. Source code, build configuration, platform and measurement scope are explicit, but these sequential three-sample synthetic runs do not establish general speedup, stable p95, cold-cache behavior or concurrent performance. The 47-second full-scan top-10 latency remains a concrete performance limitation for this workload. Profiling document decoding and vector execution, real embedding distributions, broader scales/platforms and complete resource qualification remain open.


## Combined vector-field accessor at 100,000 × 768 (2026-09-07)

The [post-change report](benchmark-results/2026-09-07-linux-release-vector-field-768-100000.json) completed at clean implementation commit `3cac94cae`, using the same release profile, Rust 1.88.0, seeded fixture and unchanged harness. Command: `python3 romedb/scripts/benchmark.py --binary target/release/romedb-cli --rows 100000 --dimensions 768 --samples 3 --fixture seeded --output romedb/docs/benchmark-results/2026-09-07-linux-release-vector-field-768-100000.json`. The rebuilt binary hash matches `cb9e8b99209bf3cef1c589a78113414740407181e7faa1c5b5cec0fcf71dc081`.

| Workload | Median before | Median after | After sample range |
|---|---:|---:|---:|
| Unindexed filter | 9.48 s | 9.26 s | 9.16–9.44 s |
| Indexed filter | 138.64 ms | 117.97 ms | 114.72–122.31 ms |
| Exact cosine top-10 | 47.12 s | 32.71 s | 32.60–32.76 s |

Warmup and all measured results passed count, ordering, uniqueness, cutoff membership and per-distance reference checks. The independent cosine reference and checkpointed database size (1,247,805,440 bytes) match the baseline. Loading took 192.68 seconds and index construction 12.36 seconds. Report medians, binary/commit identity and repeated counters were independently checked.

Vector primary VM steps fell from 1,200,080 to 1,100,080, consistent with one removed scalar-function call per document. Vector rows read remain 100,000 with 99,999 full-scan steps and one sort. Filter counters remain unchanged. Median top-10 time is 30.6% lower in these sequential runs; unchanged filter timings also vary, so avoid treating the comparison as a universal speedup. The query still takes over 32 seconds on this synthetic workload.

VmHWM observations are 2,622,386,176 then 2,620,030,976 bytes, retaining the earlier platform-accounting decrease. No memory improvement is established. Three warm samples, one machine, synthetic vectors and a single process do not establish stable p95, real-workload performance, cold/concurrent behavior or complete resource qualification. Whole-document decoding and broader V1 gates remain open.

## Million-vector release evidence

The [V1 million-vector record](v1-million-vector-evidence.md) and its raw report
remain because they support the released V1 capacity claim. They do not establish
interactive latency or the performance of later RomeDB releases.
