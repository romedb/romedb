# V2 ANN search

The frontend integrates USearch 2.26.2 without default SIMD/OpenMP features.
No new core exception is involved. The native contract below is included in
the published [2.0.0 release](release-2.0.0.md). Initial feature qualification is
retained in [evidence](v2-ann-evidence.md); the release record contains combined
acceptance and exact-artifact results.

```sql
CREATE TABLE items;
INSERT INTO items {id:items:a,v:vector32('[1,0]')};
CREATE SEARCH INDEX items_vec ON items(v)
USING VECTOR WITH (dimensions=2,metric='cosine');
SELECT id,distance FROM search::vector('items_vec',vector32('[1,0]'),10)
ORDER BY distance,id;
```

## Adopted native contract

- One field path, dimensions 1–4096, `cosine` or Euclidean `l2`. Non-null values
  must use dense float32 vector encoding and match dimensions; null/missing
  fields are absent from the graph. Cosine requires nonzero norm. L2 component
  magnitude is limited to 1e15 to keep native float32 squared distances finite.
  All supported writes validate evaluated values before publishing changes.
- The graph stores float32 values; cosine vectors are normalized for candidate
  search. Candidates are reranked using float64 arithmetic over original
  float32 components. Cosine returns 1 minus cosine similarity; L2 returns
  Euclidean distance, not squared distance. Lower distances rank first.
- HNSW connectivity 32, construction expansion 200, query expansion 512. Search
  requests up to four times the public limit, reranks and sorts that candidate
  set by distance then stored typed-ID bytes. Approximate membership is not a
  guarantee of globally exact tie selection. An unchanged graph/query has stable
  candidate ordering under the qualified build; no cross-platform bitwise graph
  guarantee is made. Use an outer ORDER BY for presentation order.
- Public limit 0–10000; index, query and limit must be immutable literals,
  parameters or an allowed vector32 constructor. Outer WHERE/join filtering
  happens after selecting the ranked slice and can return fewer results.
- The graph snapshot, SHA-256 checksum, redo events and record mapping are
  ordinary protected engine tables updated in the same transaction as documents.
  A graph checkpoint compacts and serializes after 1024 insert/delete events.
  Bulk index creation reserves once and constructs one initial graph. Later
  capacity grows geometrically with one native worker per connection. Ordinary writes append redo
  records; checkpoint writes and cold loads scale with the graph size.
- Each connection retains at most one cached graph. It replays bounded redo
  events in the current database snapshot. Checkpoint generation and per-event
  random tokens prevent reuse after transaction rollback, sequence reuse or
  branching after a savepoint. No index sidecar files or shared mutable graph
  exist. Searches after cache eviction/reopen load and verify the whole graph.
- `EXPLAIN QUERY PLAN` shows the materialized `__romedb_ann_hnsw_hits` result.
  The frontend performs HNSW search during preparation, then fetches only its
  candidate records by rowid. Engine VM counters exclude graph loading, replay,
  candidate lookup and HNSW work during preparation; do not interpret those
  counters as total ANN cost. There is no exact collection-scan fallback.
- Integrity audits reload the graph, check its checksum, cardinality and each
  stored vector against managed rows, alongside document/index agreement.
  This does not prove every HNSW edge has optimal topology or universal recall.
- Cancellation checks occur between graph mutations and before/after native
  calls using the existing progress hook. A native search/load/serialization
  call is not preempted; no hard cancellation latency or heap bound is promised.
- Catalog version 3 records dimensions, metric and the exact adapter format
  `usearch-2.26.2-f32-v1`. Identical IF NOT EXISTS definitions succeed; changed
  paths/dimensions/metrics reject. Field declarations must agree with dimensions.

## Platform and release qualification

Focused recovery/cancellation/client checks, combined RomeDB acceptance and
end-to-end recall/timing evidence pass. Linux x64 artifacts and dependency notices
are published with 2.0.0, including the native C++ dependency. Browser/WASM is
outside this release. These tests do not promise arbitrary dataset sizes or
constant-memory search; follow [operating guidance](operations.md) and the
candidate's measured workload envelope. Native Rust APIs expose
create_vector_index and search_vectors; SQL syntax is shared by CLI and all
seven native clients.

Implementation sources: [USearch repository](https://github.com/unum-cloud/USearch),
[buffer APIs](https://docs.rs/usearch/latest/usearch/struct.Index.html).
Versioned crate source and Cargo.lock determine the actual build. Preliminary
native-only probes are retained under docs/probes; they do not replace measured
RomeDB frontend behavior.
