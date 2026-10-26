# Spatial upgrade evidence

Development evidence gathered before the 2.5.0 release. Nothing here is a latency
promise; release qualification is in [the release record](release-2.5.0.md). Contract: [spatial-upgrade.md](spatial-upgrade.md).
Checklist: [spatial-upgrade-tasks.md](spatial-upgrade-tasks.md).

Host for every measurement: Intel Xeon E5-2673 v4, WSL2, single-threaded queries,
run through `scripts/romedb-heavy` (four-CPU quota, 24 GiB memory ceiling), Rust
1.88.0, `romedb-production` profile (optimized, debug assertions and overflow
checks on). Datasets are generated from fixed seeds inside the tests named below.

## S1. Candidate index selection

### Decision

The two-dimensional index uses **Morton (Z-order) keys over 31-bit quantized
longitude and latitude**, stored in an ordinary B-tree with the original float64
coordinates, and searched by conservative quadtree covers of ordered key ranges.
The **H3 prototype is rejected**. The released latitude-ordered format remains
readable and searchable.

Reasons, from the measurements below:

- Morton covers had **zero missed results** across every workload and query type,
  and the completeness argument is simple: quantization is monotone, so a closed
  coordinate interval maps into its quantized interval, and covers only add keys.
- Morton reduced entries read by one to four orders of magnitude where the
  latitude index is weakest (same-latitude data, wide viewports, thin polygons),
  and stayed close to the result count on uniform and sparse data.
- Work stayed constant as unrelated distant points were added.
- H3 needed 90 to 170 cell lookups per radius query against 7 to 15 Morton
  ranges, read up to 2.4 times more candidates in dense cities, and needed three
  stored resolutions (three index entries per point) to serve 1 to 25 km radii.
- H3 radius coverage relied on an empirical neighbor count. The pinned `h3o`
  build has no conservative rectangle or polygon cover; its polygon fill assigns
  cells by center, which can miss points. A proof for pentagons and distortion
  would be new RomeDB-specific geometry, which removes H3's main advantage.

### In-memory comparison

Test: `spatial::evaluation::candidate_index_evaluation` (ignored by default).

```sh
cargo test --locked --profile romedb-production -p romedb --lib candidate_index_evaluation -- --ignored --nocapture
```

Each row averages the queries for that workload. Results are the true matches.
Candidates are index entries read before refinement; Worst is the largest single
query. Seeks are key ranges (Morton), cells (H3) or one range (latitude). Misses
must be zero. Morton uses a 64-range cover budget; the 8- and 4-arm rows merge the
closest ranges, as statement generation does. H3 stores resolutions 5, 7 and 9.
Nearest rows double the radius from 100 m each round (the shipped source multiplies
by 4 after an empty round); Misses counts queries whose
ordered result differed from the exhaustive top k.

| Workload | Query | Method | Results | Candidates | Worst | Seeks | Misses |
| --- | --- | --- | --- | --- | --- | --- | --- |
| uniform 200k | radius 1 km | latitude | 0 | 25 | 40 | 1 | 0 |
| uniform 200k | radius 1 km | morton | 0 | 0 | 1 | 10 | 0 |
| uniform 200k | radius 1 km | morton 8 arms | 0 | 0 | 2 | 7 | 0 |
| uniform 200k | radius 1 km | morton 4 arms | 0 | 0 | 2 | 3 | 0 |
| uniform 200k | radius 1 km | h3 5/7/9 | 0 | 0 | 2 | 169 | 0 |
| uniform 200k | radius 5 km | latitude | 1 | 120 | 181 | 1 | 0 |
| uniform 200k | radius 5 km | morton | 1 | 1 | 2 | 12 | 0 |
| uniform 200k | radius 5 km | morton 8 arms | 1 | 1 | 2 | 7 | 0 |
| uniform 200k | radius 5 km | morton 4 arms | 1 | 1 | 2 | 4 | 0 |
| uniform 200k | radius 5 km | h3 5/7/9 | 1 | 1 | 3 | 127 | 0 |
| uniform 200k | radius 25 km | latitude | 1 | 609 | 840 | 1 | 0 |
| uniform 200k | radius 25 km | morton | 1 | 2 | 5 | 9 | 0 |
| uniform 200k | radius 25 km | morton 8 arms | 1 | 2 | 6 | 7 | 0 |
| uniform 200k | radius 25 km | morton 4 arms | 1 | 3 | 9 | 3 | 0 |
| uniform 200k | radius 25 km | h3 5/7/9 | 1 | 10 | 19 | 91 | 0 |
| uniform 200k | viewport 0.1 deg | latitude | 1 | 141 | 201 | 1 | 0 |
| uniform 200k | viewport 0.1 deg | morton | 1 | 1 | 2 | 12 | 0 |
| uniform 200k | viewport 1 deg | latitude | 4 | 1381 | 1824 | 1 | 0 |
| uniform 200k | viewport 1 deg | morton | 4 | 7 | 15 | 9 | 0 |
| uniform 200k | dateline viewport | latitude | 14 | 3410 | 3410 | 1 | 0 |
| uniform 200k | dateline viewport | morton | 14 | 17 | 17 | 11 | 0 |
| uniform 200k | thin diagonal polygon | latitude | 0 | 693 | 914 | 1 | 0 |
| uniform 200k | thin diagonal polygon | morton bbox | 0 | 2 | 5 | 9 | 0 |
| uniform 200k | thin diagonal polygon | morton region | 0 | 1 | 2 | 15 | 0 |
| uniform 200k | nearest k=5 | morton expansion | 5 | 30 | 56 | 100 | 0 | rounds 10.6
| uniform 200k | nearest k=50 | morton expansion | 50 | 331 | 653 | 118 | 0 | rounds 12.5
| 20 cities 200k | radius 1 km | latitude | 98 | 1003 | 1490 | 1 | 0 |
| 20 cities 200k | radius 1 km | morton | 98 | 181 | 524 | 11 | 0 |
| 20 cities 200k | radius 1 km | morton 8 arms | 98 | 204 | 622 | 7 | 0 |
| 20 cities 200k | radius 1 km | morton 4 arms | 98 | 284 | 896 | 3 | 0 |
| 20 cities 200k | radius 1 km | h3 5/7/9 | 98 | 562 | 1798 | 169 | 0 |
| 20 cities 200k | radius 5 km | latitude | 2293 | 4998 | 6694 | 1 | 0 |
| 20 cities 200k | radius 5 km | morton | 2293 | 3697 | 7102 | 12 | 0 |
| 20 cities 200k | radius 5 km | morton 8 arms | 2293 | 3995 | 7102 | 7 | 0 |
| 20 cities 200k | radius 5 km | morton 4 arms | 2293 | 4542 | 7595 | 4 | 0 |
| 20 cities 200k | radius 5 km | h3 5/7/9 | 2293 | 8738 | 9894 | 127 | 0 |
| 20 cities 200k | radius 25 km | latitude | 9966 | 11447 | 19856 | 1 | 0 |
| 20 cities 200k | radius 25 km | morton | 9966 | 9994 | 10000 | 9 | 0 |
| 20 cities 200k | radius 25 km | morton 8 arms | 9966 | 9994 | 10000 | 6 | 0 |
| 20 cities 200k | radius 25 km | morton 4 arms | 9966 | 9995 | 10000 | 3 | 0 |
| 20 cities 200k | radius 25 km | h3 5/7/9 | 9966 | 10000 | 10000 | 91 | 0 |
| 20 cities 200k | viewport 0.1 deg | latitude | 2434 | 5314 | 7304 | 1 | 0 |
| 20 cities 200k | viewport 0.1 deg | morton | 2434 | 3411 | 7238 | 12 | 0 |
| 20 cities 200k | viewport 1 deg | latitude | 10000 | 12500 | 20000 | 1 | 0 |
| 20 cities 200k | viewport 1 deg | morton | 10000 | 10000 | 10000 | 9 | 0 |
| 20 cities 200k | dateline viewport | latitude | 0 | 0 | 0 | 1 | 0 |
| 20 cities 200k | dateline viewport | morton | 0 | 0 | 0 | 11 | 0 |
| 20 cities 200k | thin diagonal polygon | latitude | 40 | 6364 | 19286 | 1 | 0 |
| 20 cities 200k | thin diagonal polygon | morton bbox | 40 | 5340 | 9987 | 8 | 0 |
| 20 cities 200k | thin diagonal polygon | morton region | 40 | 2847 | 6951 | 15 | 0 |
| 20 cities 200k | nearest k=5 | morton expansion | 5 | 31 | 132 | 27 | 0 | rounds 2.9
| 20 cities 200k | nearest k=50 | morton expansion | 50 | 297 | 723 | 43 | 0 | rounds 4.5
| same latitude 200k | radius 1 km | latitude | 4 | 114000 | 200000 | 1 | 0 |
| same latitude 200k | radius 1 km | morton | 4 | 8 | 23 | 9 | 0 |
| same latitude 200k | radius 1 km | morton 8 arms | 4 | 9 | 27 | 6 | 0 |
| same latitude 200k | radius 1 km | morton 4 arms | 4 | 12 | 38 | 3 | 0 |
| same latitude 200k | radius 1 km | h3 5/7/9 | 4 | 21 | 48 | 169 | 0 |
| same latitude 200k | radius 5 km | latitude | 50 | 200000 | 200000 | 1 | 0 |
| same latitude 200k | radius 5 km | morton | 50 | 64 | 86 | 13 | 0 |
| same latitude 200k | radius 5 km | morton 8 arms | 50 | 73 | 128 | 7 | 0 |
| same latitude 200k | radius 5 km | morton 4 arms | 50 | 103 | 165 | 3 | 0 |
| same latitude 200k | radius 5 km | h3 5/7/9 | 50 | 147 | 190 | 127 | 0 |
| same latitude 200k | radius 25 km | latitude | 256 | 200000 | 200000 | 1 | 0 |
| same latitude 200k | radius 25 km | morton | 256 | 352 | 421 | 9 | 0 |
| same latitude 200k | radius 25 km | morton 8 arms | 256 | 407 | 625 | 7 | 0 |
| same latitude 200k | radius 25 km | morton 4 arms | 256 | 618 | 1202 | 4 | 0 |
| same latitude 200k | radius 25 km | h3 5/7/9 | 256 | 872 | 1005 | 91 | 0 |
| same latitude 200k | viewport 0.1 deg | latitude | 55 | 200000 | 200000 | 1 | 0 |
| same latitude 200k | viewport 0.1 deg | morton | 55 | 70 | 118 | 13 | 0 |
| same latitude 200k | viewport 1 deg | latitude | 555 | 200000 | 200000 | 1 | 0 |
| same latitude 200k | viewport 1 deg | morton | 555 | 754 | 828 | 7 | 0 |
| same latitude 200k | dateline viewport | latitude | 0 | 0 | 0 | 1 | 0 |
| same latitude 200k | dateline viewport | morton | 0 | 0 | 0 | 11 | 0 |
| same latitude 200k | thin diagonal polygon | latitude | 0 | 64000 | 200000 | 1 | 0 |
| same latitude 200k | thin diagonal polygon | morton bbox | 0 | 365 | 420 | 9 | 0 |
| same latitude 200k | thin diagonal polygon | morton region | 0 | 61 | 132 | 15 | 0 |
| same latitude 200k | nearest k=5 | morton expansion | 5 | 26 | 76 | 43 | 0 | rounds 4.7
| same latitude 200k | nearest k=50 | morton expansion | 50 | 164 | 210 | 62 | 0 | rounds 7.0
| sparse 2k | radius 1 km | latitude | 0 | 0 | 3 | 1 | 0 |
| sparse 2k | radius 1 km | morton | 0 | 0 | 1 | 11 | 0 |
| sparse 2k | radius 1 km | morton 8 arms | 0 | 0 | 1 | 7 | 0 |
| sparse 2k | radius 1 km | morton 4 arms | 0 | 0 | 1 | 3 | 0 |
| sparse 2k | radius 1 km | h3 5/7/9 | 0 | 0 | 1 | 169 | 0 |
| sparse 2k | radius 5 km | latitude | 1 | 2 | 5 | 1 | 0 |
| sparse 2k | radius 5 km | morton | 1 | 1 | 1 | 12 | 0 |
| sparse 2k | radius 5 km | morton 8 arms | 1 | 1 | 1 | 7 | 0 |
| sparse 2k | radius 5 km | morton 4 arms | 1 | 1 | 1 | 4 | 0 |
| sparse 2k | radius 5 km | h3 5/7/9 | 1 | 1 | 2 | 127 | 0 |
| sparse 2k | radius 25 km | latitude | 1 | 7 | 14 | 1 | 0 |
| sparse 2k | radius 25 km | morton | 1 | 1 | 2 | 9 | 0 |
| sparse 2k | radius 25 km | morton 8 arms | 1 | 1 | 2 | 7 | 0 |
| sparse 2k | radius 25 km | morton 4 arms | 1 | 1 | 2 | 3 | 0 |
| sparse 2k | radius 25 km | h3 5/7/9 | 1 | 1 | 2 | 91 | 0 |
| sparse 2k | viewport 0.1 deg | latitude | 1 | 2 | 6 | 1 | 0 |
| sparse 2k | viewport 0.1 deg | morton | 1 | 1 | 1 | 12 | 0 |
| sparse 2k | viewport 1 deg | latitude | 1 | 14 | 24 | 1 | 0 |
| sparse 2k | viewport 1 deg | morton | 1 | 1 | 2 | 10 | 0 |
| sparse 2k | dateline viewport | latitude | 0 | 39 | 39 | 1 | 0 |
| sparse 2k | dateline viewport | morton | 0 | 1 | 1 | 11 | 0 |
| sparse 2k | thin diagonal polygon | latitude | 0 | 7 | 15 | 1 | 0 |
| sparse 2k | thin diagonal polygon | morton bbox | 0 | 0 | 1 | 10 | 0 |
| sparse 2k | thin diagonal polygon | morton region | 0 | 0 | 1 | 15 | 0 |
| sparse 2k | nearest k=5 | morton expansion | 5 | 32 | 98 | 133 | 0 | rounds 13.9
| sparse 2k | nearest k=50 | morton expansion | 50 | 367 | 591 | 149 | 0 | rounds 16.0
| coincident 10k | radius 1 km | latitude | 3000 | 6000 | 10000 | 1 | 0 |
| coincident 10k | radius 1 km | morton | 3000 | 5000 | 10000 | 10 | 0 |
| coincident 10k | radius 1 km | morton 8 arms | 3000 | 5000 | 10000 | 6 | 0 |
| coincident 10k | radius 1 km | morton 4 arms | 3000 | 5000 | 10000 | 3 | 0 |
| coincident 10k | radius 1 km | h3 5/7/9 | 3000 | 10000 | 10000 | 169 | 0 |
| coincident 10k | radius 5 km | latitude | 10000 | 10000 | 10000 | 1 | 0 |
| coincident 10k | radius 5 km | morton | 10000 | 10000 | 10000 | 15 | 0 |
| coincident 10k | radius 5 km | morton 8 arms | 10000 | 10000 | 10000 | 7 | 0 |
| coincident 10k | radius 5 km | morton 4 arms | 10000 | 10000 | 10000 | 4 | 0 |
| coincident 10k | radius 5 km | h3 5/7/9 | 10000 | 10000 | 10000 | 127 | 0 |
| coincident 10k | radius 25 km | latitude | 10000 | 10000 | 10000 | 1 | 0 |
| coincident 10k | radius 25 km | morton | 10000 | 10000 | 10000 | 12 | 0 |
| coincident 10k | radius 25 km | morton 8 arms | 10000 | 10000 | 10000 | 8 | 0 |
| coincident 10k | radius 25 km | morton 4 arms | 10000 | 10000 | 10000 | 4 | 0 |
| coincident 10k | radius 25 km | h3 5/7/9 | 10000 | 10000 | 10000 | 91 | 0 |
| coincident 10k | viewport 0.1 deg | latitude | 10000 | 10000 | 10000 | 1 | 0 |
| coincident 10k | viewport 0.1 deg | morton | 10000 | 10000 | 10000 | 13 | 0 |
| coincident 10k | viewport 1 deg | latitude | 10000 | 10000 | 10000 | 1 | 0 |
| coincident 10k | viewport 1 deg | morton | 10000 | 10000 | 10000 | 11 | 0 |
| coincident 10k | dateline viewport | latitude | 0 | 0 | 0 | 1 | 0 |
| coincident 10k | dateline viewport | morton | 0 | 0 | 0 | 11 | 0 |
| coincident 10k | thin diagonal polygon | latitude | 0 | 4500 | 10000 | 1 | 0 |
| coincident 10k | thin diagonal polygon | morton bbox | 0 | 8500 | 10000 | 9 | 0 |
| coincident 10k | thin diagonal polygon | morton region | 0 | 7000 | 10000 | 15 | 0 |
| coincident 10k | nearest k=5 | morton expansion | 5 | 15000 | 20000 | 46 | 0 | rounds 4.7
| coincident 10k | nearest k=50 | morton expansion | 50 | 14000 | 20000 | 50 | 0 | rounds 5.2

Adding distant uniform points around one fixed 10,000-point city (5 km radius):

| Distant points | latitude 5 km | morton 5 km | results |
| --- | --- | --- | --- |
| 0 | 6423 | 6022 | 3467 |
| 100000 | 6428 | 5877 | 3419 |
| 1000000 | 7102 | 5971 | 3449 |

### SQL-level measurement

Test: `spatial::evaluation::sql_workload_evaluation` (ignored by default), 100,000
documents per dataset, 20 query centers per row, statements generated with at
most 8 UNION ALL arms. Rows read and seeks come from the engine's query metrics.
Nearest rows show only the final injected statement; the radius rounds run before
it and are not yet included in profiles.

| Workload and query | Mean latency | Rows | Rows read | B-tree seeks | VM steps | Full-scan steps |
| --- | --- | --- | --- | --- | --- | --- |
| uniform: load 100000 documents 38.412777325s, build index 7.756182385s | | | | | | |
| uniform morton near 1000 m | 1.320233ms | 0 | 9 | 8 | 87 | 0 |
| uniform morton near 5000 m | 1.248557ms | 1 | 9 | 8 | 94 | 0 |
| uniform morton bbox 0.1 deg | 1.013752ms | 1 | 9 | 8 | 97 | 0 |
| uniform morton polygon 0.1 deg | 1.013953ms | 1 | 9 | 8 | 81 | 0 |
| uniform morton nearest k=5 | 5.491924ms | 5 | 5 | 0 | 115 | 0 |
| uniform morton nearest k=50 | 8.238948ms | 50 | 50 | 0 | 970 | 0 |
| uniform latitude near 1000 m | 635.814µs | 0 | 29 | 15 | 122 | 0 |
| uniform latitude near 5000 m | 790.23µs | 1 | 126 | 63 | 467 | 0 |
| uniform latitude bbox 0.1 deg | 657.737µs | 1 | 139 | 70 | 509 | 0 |
| uniform latitude polygon 0.1 deg | 654.344µs | 1 | 139 | 70 | 507 | 0 |
| uniform latitude nearest k=5 | 8.95633ms | 5 | 5 | 0 | 115 | 0 |
| uniform latitude nearest k=50 | 22.881587ms | 50 | 50 | 0 | 970 | 0 |
| uniform scan near 5000 m | 1.304002339s | 1 | 100000 | 0 | 600019 | 99999 |
| 20 cities: load 100000 documents 39.407664352s, build index 7.460938298s | | | | | | |
| 20 cities morton near 1000 m | 1.825634ms | 45 | 175 | 91 | 1030 | 0 |
| 20 cities morton near 5000 m | 8.472701ms | 1002 | 3457 | 1732 | 20451 | 0 |
| 20 cities morton bbox 0.1 deg | 6.643038ms | 1246 | 3662 | 1835 | 18532 | 0 |
| 20 cities morton polygon 0.1 deg | 6.331482ms | 1246 | 3662 | 1835 | 18517 | 0 |
| 20 cities morton nearest k=5 | 2.734921ms | 5 | 5 | 0 | 115 | 0 |
| 20 cities morton nearest k=50 | 5.508016ms | 50 | 50 | 0 | 970 | 0 |
| 20 cities latitude near 1000 m | 1.860281ms | 45 | 1006 | 503 | 4032 | 0 |
| 20 cities latitude near 5000 m | 6.790824ms | 1002 | 4772 | 2386 | 27743 | 0 |
| 20 cities latitude bbox 0.1 deg | 6.648137ms | 1246 | 5236 | 2618 | 25822 | 0 |
| 20 cities latitude polygon 0.1 deg | 6.381283ms | 1246 | 5236 | 2618 | 25820 | 0 |
| 20 cities latitude nearest k=5 | 2.664579ms | 5 | 5 | 0 | 115 | 0 |
| 20 cities latitude nearest k=50 | 6.93302ms | 50 | 50 | 0 | 970 | 0 |
| 20 cities scan near 5000 m | 1.293144638s | 1002 | 100000 | 0 | 603023 | 99999 |
| same latitude: load 100000 documents 33.232570915s, build index 6.540394861s | | | | | | |
| same latitude morton near 1000 m | 1.31119ms | 2 | 17 | 12 | 129 | 0 |
| same latitude morton near 5000 m | 1.581059ms | 23 | 82 | 45 | 540 | 0 |
| same latitude morton bbox 0.1 deg | 1.060696ms | 27 | 87 | 47 | 489 | 0 |
| same latitude morton polygon 0.1 deg | 1.132229ms | 27 | 87 | 47 | 474 | 0 |
| same latitude morton nearest k=5 | 3.69863ms | 5 | 5 | 0 | 115 | 0 |
| same latitude morton nearest k=50 | 7.286672ms | 50 | 50 | 0 | 970 | 0 |
| same latitude latitude near 1000 m | 44.727255ms | 2 | 120000 | 60001 | 420046 | 0 |
| same latitude latitude near 5000 m | 74.877505ms | 23 | 200000 | 100001 | 700276 | 0 |
| same latitude latitude bbox 0.1 deg | 73.978166ms | 27 | 200000 | 100001 | 700182 | 0 |
| same latitude latitude polygon 0.1 deg | 45.045384ms | 27 | 200000 | 100001 | 700180 | 0 |
| same latitude latitude nearest k=5 | 134.31396ms | 5 | 5 | 0 | 115 | 0 |
| same latitude latitude nearest k=50 | 376.182171ms | 50 | 50 | 0 | 970 | 0 |
| same latitude scan near 5000 m | 1.221398508s | 23 | 100000 | 0 | 600086 | 99999 |

Findings:

- Same-latitude data, the released index's worst case: radius, viewport and
  polygon searches fell from 45 to 75 ms to 1.1 to 1.6 ms, and nearest search
  from 134 to 376 ms to 3.7 to 7.3 ms.
- Uniform data: Morton reads fewer entries but carries about 0.4 to 0.6 ms of
  fixed cost per statement for compiling several range arms.
- Dense cities, 5 km radius: Morton read 28% fewer entries but was not faster
  (8.5 ms against 6.8 ms). The covering index below resolved this.
- A single statement that joins a VALUES list of ranges was rejected: the engine
  scanned the whole index (68 ms against 0.48 ms for the UNION ALL form).
- Merging the closest Morton ranges from about 12 to 8 arms cut statement cost
  with 5 to 15% more entries read in dense data (in-memory rows above).

### Covering index

The first SQL measurement used a native index on the Morton key only, so every
candidate also read its table row to get the ID and coordinates. The shipped
format indexes `(key, longitude, latitude, id)`, so range scans refine candidates
from the index alone. Same test and host, remeasured:

| Workload and query | Mean latency | Rows | Rows read | B-tree seeks | VM steps | Full-scan steps |
| --- | --- | --- | --- | --- | --- | --- |
| uniform: load 100000 documents 35.640379674s, build index 6.934178141s | | | | | | |
| uniform morton near 1000 m | 1.410581ms | 0 | 8 | 7 | 79 | 0 |
| uniform morton near 5000 m | 1.553124ms | 1 | 8 | 7 | 85 | 0 |
| uniform morton bbox 0.1 deg | 1.333958ms | 1 | 8 | 7 | 88 | 0 |
| uniform morton polygon 0.1 deg | 1.197698ms | 1 | 8 | 7 | 73 | 0 |
| uniform morton nearest k=5 | 7.437258ms | 5 | 5 | 0 | 115 | 0 |
| uniform morton nearest k=50 | 9.109412ms | 50 | 50 | 0 | 970 | 0 |
| uniform latitude near 1000 m | 628.801µs | 0 | 29 | 15 | 122 | 0 |
| uniform latitude near 5000 m | 809.188µs | 1 | 126 | 63 | 467 | 0 |
| uniform latitude bbox 0.1 deg | 628.212µs | 1 | 139 | 70 | 509 | 0 |
| uniform latitude polygon 0.1 deg | 641.654µs | 1 | 139 | 70 | 507 | 0 |
| uniform latitude nearest k=5 | 8.118202ms | 5 | 5 | 0 | 115 | 0 |
| uniform latitude nearest k=50 | 24.420217ms | 50 | 50 | 0 | 970 | 0 |
| uniform scan near 5000 m | 1.195501567s | 1 | 100000 | 0 | 600019 | 99999 |
| 20 cities: load 100000 documents 38.035561116s, build index 7.251718042s | | | | | | |
| 20 cities morton near 1000 m | 1.726359ms | 45 | 91 | 7 | 939 | 0 |
| 20 cities morton near 5000 m | 4.852439ms | 1002 | 1732 | 7 | 18719 | 0 |
| 20 cities morton bbox 0.1 deg | 4.256814ms | 1246 | 1835 | 7 | 16697 | 0 |
| 20 cities morton polygon 0.1 deg | 4.119824ms | 1246 | 1835 | 7 | 16681 | 0 |
| 20 cities morton nearest k=5 | 3.249815ms | 5 | 5 | 0 | 115 | 0 |
| 20 cities morton nearest k=50 | 5.45178ms | 50 | 50 | 0 | 970 | 0 |
| 20 cities latitude near 1000 m | 2.592618ms | 45 | 1006 | 503 | 4032 | 0 |
| 20 cities latitude near 5000 m | 8.059007ms | 1002 | 4772 | 2386 | 27743 | 0 |
| 20 cities latitude bbox 0.1 deg | 7.52325ms | 1246 | 5236 | 2618 | 25822 | 0 |
| 20 cities latitude polygon 0.1 deg | 6.852835ms | 1246 | 5236 | 2618 | 25820 | 0 |
| 20 cities latitude nearest k=5 | 2.450222ms | 5 | 5 | 0 | 115 | 0 |
| 20 cities latitude nearest k=50 | 6.927035ms | 50 | 50 | 0 | 970 | 0 |
| 20 cities scan near 5000 m | 1.182359922s | 1002 | 100000 | 0 | 603023 | 99999 |
| same latitude: load 100000 documents 35.977141393s, build index 7.469150337s | | | | | | |
| same latitude morton near 1000 m | 1.248471ms | 2 | 12 | 6 | 117 | 0 |
| same latitude morton near 5000 m | 1.21404ms | 23 | 45 | 8 | 495 | 0 |
| same latitude morton bbox 0.1 deg | 1.013151ms | 27 | 47 | 7 | 442 | 0 |
| same latitude morton polygon 0.1 deg | 1.060663ms | 27 | 47 | 7 | 427 | 0 |
| same latitude morton nearest k=5 | 2.819933ms | 5 | 5 | 0 | 115 | 0 |
| same latitude morton nearest k=50 | 5.318322ms | 50 | 50 | 0 | 970 | 0 |
| same latitude latitude near 1000 m | 57.229069ms | 2 | 120000 | 60001 | 420046 | 0 |
| same latitude latitude near 5000 m | 91.610827ms | 23 | 200000 | 100001 | 700276 | 0 |
| same latitude latitude bbox 0.1 deg | 78.242685ms | 27 | 200000 | 100001 | 700182 | 0 |
| same latitude latitude polygon 0.1 deg | 49.256255ms | 27 | 200000 | 100001 | 700180 | 0 |
| same latitude latitude nearest k=5 | 127.914759ms | 5 | 5 | 0 | 115 | 0 |
| same latitude latitude nearest k=50 | 319.683631ms | 50 | 50 | 0 | 970 | 0 |
| same latitude scan near 5000 m | 1.174600955s | 23 | 100000 | 0 | 600086 | 99999 |

Findings:

- Dense cities, 5 km radius: 4.9 ms against 8.1 ms on the latitude index, with
  7 B-tree seeks instead of about 2,400.
- Same-latitude data: radius, viewport and polygon searches take 1.0 to 1.3 ms
  against 49 to 92 ms; nearest search takes 2.8 to 5.3 ms against 128 to 320 ms.
- Uniform data still carries 0.6 to 0.9 ms of fixed cost over the latitude index
  for selective queries that return one row. This comes from compiling up to 8
  range arms per statement and remains an open tuning item.
- Loading 100,000 documents took 35 to 38 s and building the index 7 s. Both are
  dominated by the existing document write path, measured one statement per insert.

## 2.5.1 follow-up

Measured 2026-10-13 on the same host with the `romedb-production` profile.
These are local samples, not latency or capacity guarantees.

### Nearest search with selective filters

Test: `spatial::evaluation::nearest_filter_evaluation` (ignored by default).
100,000 in-memory documents in 20 cities of about 5 km spread; one third
available, `last_seen` from 0 to 99, and 0.1% in the `night` fleet. Each row is 20
queries for the 5 nearest matching documents.

| Query | 2.5.0 mean / max | 2.5.1 mean / max |
| --- | --- | --- |
| Available and fresh | 26.9 / 108.5 ms | 29.2 / 95.5 ms |
| Night fleet, no fleet index | 885.5 / 2,960 ms | 347.4 / 595.9 ms |
| Night fleet, plain scan for reference | 243.9 / 310.7 ms | 122.5 / 154.1 ms |
| Night fleet, fleet index | 523.3 / 1,592 ms | 7.0 / 14.9 ms |
| Day fleet, fleet index | 1,082 / 1,636 ms | 23.7 / 33.0 ms |

In 2.5.0 a scalar index on the filter field made nearest search slower: the
statement drove from the scalar index and checked spatial membership for almost
every day-fleet driver. 2.5.1 counts both candidate sets and uses the smaller one
(contract section 7). Without a fleet index, rare filters still cost about three
plain scans on average, because rounds read documents before the match rate shows
that a scan is cheaper. Index such fields when nearest search filters on them.

### Field reads and result rows

Field accessors now walk the canonical stored layout with a byte scanner, stop at
the requested member or the first later key, and decode only that member.
Accessors that return encoded values pass the member's stored bytes through.
Measured over 100,000 documents, ten runs each:

| Query | 2.5.0 | 2.5.1 |
| --- | --- | --- |
| `SELECT id FROM people WHERE fleet = 'night'` | 258 ms | 125 ms |
| Three fields of 33,384 rows | 541 ms | 235 ms |
| Night-fleet nearest through the ordinary plan | 218 ms | 131 ms |

### Frozen work ceilings

Test: `selective_fixture_work_stays_under_frozen_ceilings`. 10,000 points in 20
cities, 10,000 uniform points and 200 zones from a fixed seed. Work counters are
deterministic, so the test asserts them. Columns are index entries, rows read,
key ranges and nearest rounds; ceilings are about 1.5 times the measured work.

| Query | Measured | Ceiling |
| --- | --- | --- |
| `search::near`, 1 km in a city | 43 / 51 / 8 / 0 | 65 / 80 / 8 / 0 |
| `ST_DWithin`, 1 km in a city | 88 / 143 / 8 / 0 | 135 / 215 / 8 / 0 |
| `search::bbox`, 0.02 degree | 21 / 23 / 2 / 0 | 35 / 35 / 4 / 0 |
| `ST_Covers(envelope, location)`, 0.02 degree | 52 / 87 / 2 / 0 | 80 / 130 / 4 / 0 |
| `search::nearest`, k = 5 in a city | 0 / 5 / 24 / 3 | 50 / 10 / 40 / 5 |
| Ordinary nearest, available, k = 5 | 100 / 167 / 56 / 5 | 150 / 250 / 90 / 8 |
| `ST_DWithin`, 50 km in open ocean | 0 / 3 / 3 / 0 | 10 / 10 / 6 / 0 |
| `search::nearest`, k = 5 in open ocean | 0 / 5 / 41 / 7 | 50 / 10 / 64 / 10 |
| `ST_Covers(shape, point)`, 200 zones | 31 / 47 / 1 / 0 | 50 / 75 / 2 / 0 |
| `search::containing`, 200 zones | 31 / 47 / 1 / 0 | 50 / 75 / 2 / 0 |

### Moving drivers with concurrent searches

Script: `scripts/bench-spatial-movement.cjs`, run three times through
`scripts/romedb-heavy` with the 2.5.1 release Node package. 100,000 drivers in 20
cities on one file; one writer connection moves random drivers with synced
autocommit updates while three reader connections alternate nearest and viewport
searches for 20 seconds. Each run ends with an integrity check of every document
and 20 indexed nearest results compared with a scan.

Times are p50 / p95 / p99 milliseconds.

| Run | Moves per second | Driver move (synced) | Nearest 5 available and fresh | Viewport 0.02 degree |
| --- | --- | --- | --- | --- |
| 1 | 80 | 8.6 / 28.9 / 53.2 | 39.0 / 73.4 / 101.8 | 3.8 / 6.2 / 7.2 |
| 2 | 122 | 7.6 / 10.5 / 11.8 | 40.6 / 75.4 / 118.5 | 4.0 / 6.1 / 7.2 |
| 3 | 116 | 8.2 / 10.9 / 11.9 | 38.9 / 72.5 / 105.7 | 3.9 / 6.3 / 7.3 |

The first run's move p95 of 29 ms did not repeat: the next two runs stayed under
11 ms. Its cause was not isolated; synced commits on this WSL2 disk vary between
runs. Recorded budgets on this host, with
one writer and three readers: moves at p95 under 15 ms in a typical run, nearest
search at p95 under 120 ms and viewports at p95 under 10 ms. A development build
before release measured 119 moves per second with p95 values of 10.6, 82.5 and
6.6 ms.

### Robustness

- `randomized_writes_keep_indexed_results_equal_to_scans`: 120 transactions of 25
  random inserts, updates, NULL and missing locations, deletes and zone changes,
  clustered on the antimeridian, the poles and the prime meridian, with every
  ninth transaction rolled back. Every fourth round compares radius, nearest,
  envelope, region and antimeridian viewport results with scans; final integrity
  audits pass.
- `nearest_rounds_stop_on_cancellation_deadlines_and_limits`: cancellation and
  deadlines during nearest rounds return `FDB_CANCELLED` inside an open
  transaction and leave it usable; result limits fail without partial output;
  invalid `k` values fail validation; `LIMIT 10001` keeps ordinary semantics.
- `interrupted_reindex_keeps_the_latitude_format_index`: a cancelled, rolled-back
  or uncommitted `REINDEX` of a latitude-format index leaves the old index,
  catalog version 3 and its results intact after reopening.
- The shared native client fixture adds polygon parameter round trips, region
  indexes, `search::containing`, GeoJSON output, filtered nearest search and an
  invalid polygon. PHP, Go, C# and Swift pass all 110 steps; Python and Node run
  the same tagged steps.

## 2.5.3 scans and grouping

Measured 2026-10-14 with the `romedb-production` profile on one 65 MB file-backed
database of 100,000 documents. Binaries built from 2.5.2 (`main`) and 2.5.3
alternated three times per query; the table shows the best run of ten
executions each. The host was shared with other work, so differences under
about 10% are within run-to-run variation.

| Query | 2.5.2 | 2.5.3 | Change |
| --- | --- | --- | --- |
| `SELECT id FROM people WHERE fleet = 'night'` | 139.9 ms | 115.6 ms | −17% |
| Three fields of 33,384 rows | 283.1 ms | 250.6 ms | −11% |
| `GROUP BY fleet` with `count(*)` | 260.1 ms | 238.1 ms | −8% |
| `GROUP BY fleet` with `count(*)` and `sum(last_seen)` | 517.4 ms | 395.8 ms | −24% |
| `GROUP BY geo::cell(location, 3)` | 1,120.9 ms | 845.7 ms | −25% |
| Night-fleet nearest through the ordinary plan | 152.9 ms | 130.1 ms | −15% |

Changes: field reads parse simple members without the JSON decoder; helpers
decode canonical points directly; grouped collection queries extract field
values before grouping, so the GROUP BY sorter no longer copies whole documents
or spills them to a temporary file. A ten-times larger page cache gave only 4 to
10% on the same scans and was not adopted.
