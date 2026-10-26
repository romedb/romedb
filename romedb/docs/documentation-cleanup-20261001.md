# Documentation cleanup, 2026-10-01

Removed obsolete planning/experiment instructions and intermediate benchmark output.
Historical text remains recoverable from Git. Current contracts, release records,
core-exception decisions, and their required evidence remain in the checkout.
Links to removed historical documents point here to avoid implying replacement evidence.

## Removed files

- `preview-release-draft.md`
- `accessor-performance.md`
- `benchmark-results/2026-09-07-linux-dev-reference-cyclic-1000.json`
- `benchmark-results/2026-09-07-linux-dev-parser-stack-1000.json`
- `benchmark-results/2026-09-07-linux-dev-100000.json`
- `benchmark-results/2026-09-07-linux-dev-1000.json`
- `benchmark-results/2026-09-08-linux-debug-node-writer255-1000.json`
- `benchmark-results/2026-09-07-linux-dev-seeded-768-1000.json`
- `benchmark-results/2026-09-08-linux-debug-node-binary255-1000.json`
- `benchmark-results/2026-09-07-linux-dev-transfer-json-replay-1000.json`
- `benchmark-results/2026-09-08-linux-debug-node-direct-json-1000.json`
- `benchmark-results/2026-09-07-linux-dev-profile-1000.json`
- `benchmark-results/2026-09-07-linux-dev-profile-100000.json`
- `benchmark-results/2026-09-08-linux-debug-node-inplace-1000.json`
- `benchmark-results/2026-09-08-linux-debug-node-reuse255-1000.json`
- `benchmark-results/2026-09-07-linux-dev-fetch-1000.json`
- `benchmark-results/2026-09-07-linux-release-vector-field-comparison.json`
- `benchmark-results/2026-09-07-linux-release-accessor-1000.json`
- `benchmark-results/2026-09-08-linux-debug-node-results-1000.json`
- `benchmark-results/2026-09-07-linux-dev-seeded-768-100000.json`
- `benchmark-results/2026-09-07-linux-dev-transfer-1000.json`

## Current references

Use [production performance](production-envelope.md), [benchmark harness and retained release evidence](benchmarks.md), and [2.1.0 release](release-2.1.0.md). Three 100k/1m raw reports required by V1 release evidence remain. Preview guides used by packaging scripts also remain.
