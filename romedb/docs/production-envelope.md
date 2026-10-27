# RomeDB 2.1.0 production operating envelope

These measurements qualify the Linux x64 distribution built from
`587c3b4afae7382920fcdec64e91b1a97eda6f5a`. The raw workload reports and logs under
`evidence/qualification/` bind the exact Node package, addon, build manifest,
checksums and harness. All four samples passed query/index correctness,
integrity, checkpoint, cancellation/retry, and reopen integrity and n/revision
checks for every document.

## Supported deployment boundary

The finite search qualification covers 1,000 documents with 32-dimensional
float32 vectors and 5,000 documents with 128-dimensional vectors, each with one
or four connections in one owning process. The corpus uses one approximately
256-byte indexed text field, one L2 ANN vector and one geographic point per
document. Keep initial deployments within 5,000 such indexed documents,
128 dimensions and four connections per database; larger text, additional
indexes/databases or other embedding distributions require application-specific
qualification. These are operational boundaries, not enforced engine caps or
proof that every smaller/intermediate combination has identical performance.

The concurrent configuration is one writer and three readers using Node worker
threads. Bound queues and serialize competing writes. Other application
processes must use the owner through application IPC or own separate files.
This release does not supply a network database server.

## Host and method

Ubuntu 24.04 x64 under WSL2, kernel `6.18.33.2-microsoft-standard-WSL2`; CPU
`Intel(R) Xeon(R) CPU E5-2673 v4 @ 2.30GHz` (80 logical CPUs visible),
62.73 GiB host memory, Node 24.19.0.
The optimized `romedb-production` Rust profile retains debug assertions and
overflow checks. Each of four configurations has one sample, with 5,000 serial
mixed operations: 40% document reads, 20% indexed updates, 20% text search,
10% ANN and 10% spatial. Half the text searches match the entire corpus;
returned text/vector limits are 10. `synchronous=FULL` and the engine's normal
automatic checkpoint behavior remain enabled.

The four-connection mixed phase still executes serially. A separate 1,000-round
phase overlaps one writer and three snapshot readers, 4,000 transactions in
all. It verifies each reader's document and index results refer to the same
revision and checks every acknowledged write after reopening. No local builds
ran alongside these measurements. OS caches were not evicted. This is a finite
correctness and resource qualification, not a long-duration soak, latency SLA,
maximum-throughput benchmark or claim about all Linux machines.

## Mixed operations

Latencies are milliseconds; each row identifies documents / dimensions /
connections. Throughput includes assertions and resource sampling; operation
latency includes Node/frontend/engine work and excludes later assertions.

| Corpus / dimensions / connections | Ops/s | Read p95 | Write p95 | FTS p95 | ANN p95 | Spatial p95 |
| --- | --- | --- | --- | --- | --- | --- |
| 1,000 / 32 / 1 | 46.71 | 0.93 | 116.60 | 30.22 | 6.59 | 1.20 |
| 1,000 / 32 / 4 | 40.63 | 0.96 | 127.81 | 39.46 | 7.76 | 1.28 |
| 5,000 / 128 / 1 | 22.94 | 1.03 | 285.48 | 78.59 | 14.84 | 1.42 |
| 5,000 / 128 / 4 | 20.04 | 1.06 | 294.76 | 110.42 | 18.05 | 2.15 |

Maximum observed operation latency, in the same order:

| Configuration | Read max | Write max | FTS max | ANN max | Spatial max |
| --- | --- | --- | --- | --- | --- |
| 1,000 / 32 / 1 | 1.62 | 144.36 | 44.73 | 11.32 | 2.44 |
| 1,000 / 32 / 4 | 2.30 | 163.11 | 55.25 | 11.90 | 2.12 |
| 5,000 / 128 / 1 | 2.09 | 634.74 | 109.67 | 51.08 | 2.20 |
| 5,000 / 128 / 4 | 1.88 | 391.44 | 149.66 | 55.58 | 3.83 |

## Setup, checkpoint and resources

Index and checkpoint times below are milliseconds. RSS includes the Node
runtime, setup and worker threads. Database/WAL values are observed maxima
across the phases; WAL sampling can miss peaks inside one operation.

| Configuration | FTS build | ANN build | Spatial build | Mixed checkpoint | Peak RSS MiB | Max DB MiB | Max WAL MiB |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1,000 / 32 / 1 | 151.14 | 588.08 | 96.13 | 33.92 | 150.15 | 5.03 | 4.15 |
| 1,000 / 32 / 4 | 148.86 | 598.89 | 79.57 | 44.15 | 297.49 | 7.09 | 32.40 |
| 5,000 / 128 / 1 | 1106.16 | 3505.64 | 492.34 | 39.39 | 189.09 | 30.80 | 14.85 |
| 5,000 / 128 / 4 | 894.79 | 3375.17 | 484.68 | 55.32 | 407.59 | 33.78 | 660.61 |

Maximum first-query latency across each configuration's newly opened
connections, with a warm OS cache (milliseconds):

| Configuration | Read | FTS | ANN | Spatial |
| --- | --- | --- | --- | --- |
| 1,000 / 32 / 1 | 0.91 | 3.21 | 9.15 | 0.94 |
| 1,000 / 32 / 4 | 0.75 | 3.45 | 10.40 | 1.25 |
| 5,000 / 128 / 1 | 1.59 | 7.80 | 54.21 | 1.24 |
| 5,000 / 128 / 4 | 0.89 | 5.37 | 57.27 | 1.28 |

## Concurrent readers and writer

| Documents / dimensions | Transactions/s | Writer p95 ms | Writer max ms | Largest reader p95 ms | Checkpoint ms |
| --- | --- | --- | --- | --- | --- |
| 1,000 / 32 | 17.29 | 338.23 | 433.14 | 109.44 | 38.56 |
| 5,000 / 128 | 9.59 | 531.78 | 732.02 | 166.45 | 63.01 |

1,000 documents: all 4,000 transactions completed; natural Busy/BusySnapshot counts 0, transaction restarts 0, commit retries 0. The separate forced-contention probe observed 1 Busy error and a successful retry after confirmed lock release (142.81 ms).

5,000 documents: all 4,000 transactions completed; natural Busy/BusySnapshot counts 0, transaction restarts 0, commit retries 0. The separate forced-contention probe observed 1 Busy error and a successful retry after confirmed lock release (265.66 ms).

## Headroom and practical limits

The largest observed process RSS was 407.59 MiB. Start with at least
1 GiB available for the database-owning process, plus separate capacity
for the application and OS. This is a rounded planning allowance of at least
twice the observed peak, not a tested memory limit. Reserve at least
4 GiB of free local storage for this small workload, plus database
growth and complete backups; this rounds up at least four times the largest
observed database-plus-WAL maxima. Neither budget is an engine-enforced cap.
Monitor actual RSS, disk/WAL size and tail latency; requalify before increasing
corpus size, text length, index count, connections or queued work.

FTS allocates collector scratch proportional to corpus size and materializes
all matches before applying the public limit. A small result limit does not
bound that work. ANN retains a graph per connection and can reload/serialize
the whole graph. The mixed workload slows over the measured run; the raw series records
that trend without attributing it to a specific operation kind. Do not extrapolate early-run
throughput to indefinite operation or promise constant-memory top-k search.
Long-lived or continuously overlapping readers can delay WAL reuse; the
automatic checkpoint threshold is not a WAL size cap. Complete transactions promptly and
monitor checkpoint results. The explicit truncate checkpoints passed with
`[0,0,0]`, with WAL zero at the recorded checkpoint boundary.

The VM cancellation probe requested a 20 ms deadline. Measured response times
were 36.35–48.13 ms, followed by successful retries
and preserved caller transactions. Native FTS/ANN calls are not guaranteed to
stop at that deadline. Synthetic ANN recall was checked against exact generated
neighbors; see the raw reports for values. This distribution does not establish
recall for arbitrary real application embeddings.

Preserve maintenance-window backups and test restores. These results complement
the installed seven-client, SQLite adoption, prior-version upgrade/restore and
native fault regressions; they do not add multiprocess ownership, online backup,
a power-loss hardware guarantee, graph database or cloud support.
