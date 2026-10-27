# V2 search implementation assessment

Updated 2026-09-25. V2-F and V2-A are qualified under their native contracts.
Neither is released.

## Full-text implementation and approved engine correction

The pinned engine contains an experimental Tantivy index method in
`core/index_method/fts.rs`, gated by `fts`; Cargo.lock pins Tantivy 0.26.1.
The working-tree frontend enables it and opts into index methods. No existing
locked dependency was upgraded. The Rust 1.88 build passed.

Managed `CREATE SEARCH INDEX ... USING FULLTEXT` and
`search::text(index, query, limit)` now cover protected text storage, validation
on common writes, transactional counts, catalog version 3, lifecycle, schema
ownership, logical integrity, typed IDs and score ordering. Native custom-index
DDL remains unavailable through ordinary public SQL. See the
[working contract](v2-fulltext.md) for grammar, tokenizer, filtering and costs.

Two native issues were reproduced:

1. **Premature tie truncation.** Native `ORDER BY score DESC,id LIMIT n` limits
   inside FTS before ID sorting. The frontend materializes all matching hits with
   an explicit transactional document-count limit, then applies stable ordering
   and the public limit. This also avoids the native implicit million-hit cap.
   The 40-way tie regression passes and EXPLAIN confirms native index use.
2. **Cross-connection cache visibility.** A reader can see uncommitted writer hits
   and changed scores after the writer queries its changes. The shared directory
   retains the originating pager; its existing consistency check consults that
   pager, not the requesting connection's snapshot. The
   [approved focused core patch](proposals/fts-cache-snapshot.md) restricts reuse
   to the requesting pager. It is integrated in isolated commit `fb246a8e4`,
   with provenance and regression coverage. All 26 native FTS tests pass.

The standalone control fails on unmodified core and passes with the patch,
including a reader snapshot held across commit and both rollback forms. Managed
coverage now includes multi-connection isolation, atomic build/write/drop,
unique replacement, UPSERT, failed INSERT SELECT, abrupt process exit,
cancellation, integrity, parser grammar and synchronous/asynchronous clients.
See [full-text evidence](v2-fulltext-evidence.md) for current combined acceptance
results and measured native index selectivity.

The pinned query parser also produces negative-only subclauses for forms such
as `fast AND NOT guide`, yielding an incorrect empty result. The frontend rejects
these AST shapes explicitly; `+fast -guide` is supported and tested. This does
not require another core exception.

Dependency declarations and crate notices were refreshed: 259 declarations,
252 verified registry archives and 168 distinct notice texts. The 32 inventory
entries without collected filename candidates still need the existing
supplements and the separate V2-R attribution review.

The native module is excluded on `target_family = "wasm"`. Resolve browser
capabilities under V2-C before release; neither FTS nor WASM is removed from V2.

## ANN: the built-in method is not the dense search contract

`core/index_method/toy_vector_sparse_ivf.rs` implements a sparse-vector inverted
index for Jaccard queries. It does not supply the dense cosine/L2 contract.
V2-A instead adopts USearch through the public engine interfaces, with dimensions,
metrics, ties, post-filters and persistence defined in [the ANN contract](v2-ann.md).
No ANN core exception is required.

### Candidate screening (2026-09-25; USearch selected and qualified for native V2-A)

- `instant-distance` 0.6.1 offers a small pure-Rust HNSW map with build/search
  APIs. Its repository was archived on 2026-07-23. Treat maintenance and
  immutable-map write amplification as adoption concerns, not as a qualified
  transactional database index. Sources: [repository](https://github.com/djc/instant-distance),
  [versioned API](https://docs.rs/instant-distance/0.6.1/instant_distance/struct.HnswMap.html).
- `hnsw_rs` 0.3.4 documents dump/reload through separate graph and data files.
  That persistence interface alone does not establish atomicity with our engine
  transaction or savepoints. A candidate must demonstrate a single-database
  persistence adapter or an explicit recovery protocol before adoption.
  Source: [versioned persistence API](https://docs.rs/hnsw_rs/0.3.4/hnsw_rs/hnswio/index.html).

Selection required mutable dense search, byte serialization, snapshot-safe
persistence, cancellation boundaries, Rust 1.88 compatibility, explicit filter
semantics and exact-search comparisons. Native evidence is now recorded below;
WASM capability and broader release qualification remain open. Custom vector
algorithms and compression research stay in V3.

USearch 2.26.2 is now integrated through its Rust/C++ buffer API, with default
features disabled and no additional core changes. It supports mutable dense
HNSW, deletion, compaction and memory serialization; our adapter persists graph
snapshots and a bounded redo log in the same engine transaction as documents.
The initial native 10k-vector probe passed restore/mutation and recall checks.
A 100k-vector held-out-query probe exposed 89.1% L2 recall@10 with expansion 128;
expansion 512 returned 99.5% L2 and 99.8% cosine recall on that same fixture.
Frontend qualification also reproduced that reserving per insertion resets
native search contexts and produces a flat graph: 0 upper-layer nodes out of 256,
versus 10 with one upfront reservation. The adapter now reserves once for bulk
builds and grows capacity geometrically; a topology regression protects this. See [the working ANN contract](v2-ann.md)
and [USearch APIs](https://docs.rs/usearch/latest/usearch/struct.Index.html).
[Native V2-A acceptance](v2-ann-evidence.md) now passes combined Rust/Node checks
and the measured frontend comparison. Platform/WASM and release artifacts remain
V2-C/V2-R work.
