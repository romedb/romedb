# V2 full-text search

Current source integrates Turso 0.8.1. Its explicit FTS migration,
catalog version 4, concurrent transactions, and rejection of trailing DML LIMIT
supersede the older descriptions below; published release artifacts are unchanged.

Implemented and qualified for the documented native contract. The approved
[cache isolation fix](proposals/fts-cache-snapshot.md) is integrated as
`fb246a8e4`. Later V1 upgrade checks exposed backing-storage integrity and cleanup
bugs; the separately approved [storage fix](proposals/fts-integrity.md) is
integrated as `12109384a`. The affected native suites and complete V1 upgrade and
restore rehearsal now pass, closing the reopened V2-F gate. See
initial search verification and
integrated storage verification.
The scalar-error correction is also integrated, and combined native acceptance
passed for the published 2.0.0 release. Browser/WASM is outside
the native release. The limits below remain part of the supported contract.

```sql
CREATE TABLE articles;
INSERT INTO articles {id:articles:a,title:'Fast database',body:'Atomic updates'};
CREATE SEARCH INDEX articles_text ON articles(title, body) USING FULLTEXT;

SELECT id, score
FROM search::text('articles_text', 'database', 20)
ORDER BY score DESC, id;

SELECT a.title, h.score
FROM search::text('articles_text', 'database', 20) h
JOIN articles a ON a.id=h.id
ORDER BY h.score DESC, h.id;
```

## Adopted contract

- One to 16 distinct ordered field paths. Values must be strings, missing or
  NULL. Missing/NULL fields contribute no terms. Every evaluated write validates
  every indexed path. Index names use ASCII letters, digits and underscores,
  canonicalized to lowercase, because native directory DDL does not quote them.
- `IF NOT EXISTS` accepts an identical managed definition on the same collection;
  it does not accept a different field list or search kind. `DROP INDEX`, table
  drop, builds, writes and catalog changes use statement savepoints.
- Catalog version 4 persists the ordered paths and `tantivy-default-0.26`
  tokenizer identity. The engine uses Tantivy 0.27.0, whose default tokenizer is
  unchanged from 0.26, so the identity stays the same. That tokenizer: split on punctuation, lowercase and omit tokens of 40 bytes
  or longer. No stemming, accent folding or language-specific segmentation is promised.
- Queries apply across all indexed text fields. Supported grammar: terms,
  quoted phrases, prefix phrases, boolean AND/OR, parentheses, required and
  prohibited terms, and `*`. Query input is limited to 4096 UTF-8 bytes and AST
  nesting 32. Field qualifiers, boosts, regexes, sets and ranges reject explicitly.
  Bare terms combine with OR by default. Since the source after 0.2.20, `hel*` finds
  documents with any indexed word that starts with `hel` after the index's analyzer
  runs, so `HEL*` is the same query. `"hel"*` is the same as `hel*`, and a
  phrase prefix of two or more words (for example `"big bad wo"*`) is unchanged. A
  prefix combines like a word: `+fast +hel*`, `hel* -help` and inside
  parentheses. It needs at least 2 characters before the `*`, and takes one `*`
  at the end of a word (`hel**` and `he*l*` reject). It matches at most the 64
  indexed words that are in the most documents, and each scores with BM25 as if
  the query named that word, so `hel*` scores a document that contains `hello`
  as `hello` would. On an ngram index `hel*` is the same as `hel`, because an
  ngram index already matches parts of words. A prefix with no matching word
  matches nothing, also inside `+fast +xyz*`. Negative-only clauses reject, including
  the pinned parser's `NOT` subclauses: use `+fast -guide` to exclude a term,
  rather than `fast AND NOT guide`, which the native parser mishandles. Matching
  and syntax errors otherwise follow the pinned Tantivy query parser.
- `WITH (weights='title=3,meta.summary=2')` multiplies each named field's score
  (source after 0.2.20). Each key is one of the index's paths, written with dots;
  each value is a finite, positive number; paths left out weigh 1. Weights are part
  of the definition: `IF NOT EXISTS` with other weights rejects, `INFO FOR TABLE`
  lists `weights` in path order, and `REINDEX`, reopening and file exports keep
  them. A weighted index raises its collection to catalog version 9, which earlier
  binaries refuse with "unsupported collection metadata version". Weights that are
  all 1 store nothing and leave the catalog version unchanged. To change weights,
  drop the index and create it again.
- `search::text(index, query, limit)` requires immutable literal or parameter
  arguments and a limit from 0 through 10000. It returns typed `id` and native
  numeric BM25 `score`. Higher scores rank first; equal scores use ascending
  stored typed-ID encoding, not numeric-key ordering. Use an outer `ORDER BY`
  when presentation order matters. Scores depend on the indexed corpus.
- `search::snippet(index, text, query, size)` (source after 0.2.20) returns the
  part of `text` that best shows a match for `query`, as an array of
  `{ text, match }` objects, where `match` is true on the words the query
  matched. It returns plain text, never markup, so the caller decides how to
  show a match. It analyzes `text` with the named index's tokenizer, so matching
  follows the index: a phrase matches its words in order, a prefix matches the
  words that start with it, words in a negative clause are not marked, and on an
  ngram index each place the query text appears is marked, ignoring case. A
  phrase with slop (`"quick fox"~1`) marks its words one by one. The fragment has
  at most `size` characters (40 through 1000) and starts and ends at whole words
  when the text has word breaks. It is the fragment with the most different query
  words, then the most matches, then the earliest; when that fits in the first
  `size` characters it starts at the beginning of the text. Runs of whitespace
  become one space, and a cut start or end shows as `…` in the first or last
  part. With no match the start of the text comes back as one part with `match`
  false; empty text gives an empty array. Null text or a null query gives null.
  The index name must be a string literal; text, query and size may be bound
  values or expressions. The query follows the rules and errors of
  `search::text`, and text is limited to 1 MiB. The Rust API is
  `Connection::snippet_text`.
- The limit selects ranked hits before an outer `WHERE` or join filter. Such
  filters may produce fewer than the requested count. This is not pre-filtered
  top-k search. The materialized hit set prevents predicate/limit pushdown from
  changing this contract.
- The native plan must select the FTS index or preparation fails. There is no
  text-scan fallback. Since 0.2.18 a search without allowed IDs asks the native
  index for its best hits in score order, twice the limit plus 16, and doubles
  that window until every hit that ties with the last one wanted is inside it.
  It reads them once while the statement is planned and gives them to the
  statement as rows, as `search::nearest` does, so the statement runs in one
  snapshot. Cost grows with the size of the tie at the limit, not with the
  number of matches. A search with allowed IDs still retrieves every live
  match (native LIMIT -1) and materializes it before the filter, stable sorting
  and the public limit, so its cost can grow with the match count.
- Collection integrity checks compare source documents with managed text fields,
  IDs and the count. They do not prove every native Tantivy posting is intact.
  Owned schema and the native directory are checked on connection open;
  managed/internal storage cannot be accessed through public SQL.
- All seven native clients and SQL/CLI share the parser and frontend. Browser/
  WASM support was removed from the active checkout and is not a release gate.

## Historical native issues and qualification (0.7.2)

Native `ORDER BY score DESC,id LIMIT n` can return arbitrary members of a score
tie because it limits in the FTS method before ID sorting. The frontend avoided
this with an explicitly materialized complete hit set until 0.2.18, and now
widens the native window past the whole tie instead. A 40-document tie fixture
returns the same first three typed IDs as the fully ordered result, and a
3,000-document fixture with ties of 30 and about 430 matches the full ordering
at limits from 1 to 500 while reading under 200 rows.

The native shared directory cache can retain a different connection's pager.
The approved fix restricts reuse to the requesting pager before the existing
rollback check. The native regression covers reader isolation, a pinned snapshot
across commit, savepoint rollback and full rollback. Managed/client acceptance
is recorded in verification. The V2 checklist
is complete; current operating guidance is in [deployment](deployment.md) and
[operations](operations.md).
