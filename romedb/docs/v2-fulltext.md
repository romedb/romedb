# V2 full-text search

Current source integrates [Turso 0.8.1](turso-0.8.1.md). Its explicit FTS migration,
catalog version 4, concurrent transactions, and rejection of trailing DML LIMIT
supersede the older descriptions below; published release artifacts are unchanged.

Implemented and qualified for the documented native contract. The approved
[cache isolation fix](proposals/fts-cache-snapshot.md) is integrated as
`fb246a8e4`. Later V1 upgrade checks exposed backing-storage integrity and cleanup
bugs; the separately approved [storage fix](proposals/fts-integrity.md) is
integrated as `12109384a`. The affected native suites and complete V1 upgrade and
restore rehearsal now pass, closing the reopened V2-F gate. See
[initial search verification](v2-fulltext-evidence.md) and
[integrated storage verification](v2-upgrade-restore-evidence.md).
The scalar-error correction is also integrated, and combined native acceptance
passed for the published [2.0.0 release](release-2.0.0.md). Browser/WASM is outside
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
  tokenizer identity. The pinned engine uses Tantivy 0.26.2 with its default
  tokenizer: split on punctuation, lowercase and omit tokens of 40 bytes
  or longer. No stemming, accent folding or language-specific segmentation is promised.
- Queries apply across all indexed text fields. Supported grammar: terms,
  quoted phrases, prefix phrases, boolean AND/OR, parentheses, required and
  prohibited terms, and `*`. Query input is limited to 4096 UTF-8 bytes and AST
  nesting 32. Field qualifiers, boosts, regexes, sets and ranges reject explicitly.
  Bare terms combine with OR by default. Phrase prefixes require at least two
  terms (for example `"big bad wo"*`). Negative-only clauses reject, including
  the pinned parser's `NOT` subclauses: use `+fast -guide` to exclude a term,
  rather than `fast AND NOT guide`, which the native parser mishandles. Matching
  and syntax errors otherwise follow the pinned Tantivy query parser.
- `search::text(index, query, limit)` requires immutable literal or parameter
  arguments and a limit from 0 through 10000. It returns typed `id` and native
  numeric BM25 `score`. Higher scores rank first; equal scores use ascending
  stored typed-ID encoding, not numeric-key ordering. Use an outer `ORDER BY`
  when presentation order matters. Scores depend on the indexed corpus.
- The limit selects ranked hits before an outer `WHERE` or join filter. Such
  filters may produce fewer than the requested count. This is not pre-filtered
  top-k search. The materialized hit set prevents predicate/limit pushdown from
  changing this contract.
- The native plan must select the FTS index or preparation fails. There is no
  text-scan fallback. Native LIMIT -1 retrieves every live indexed match without
  a shared document counter. All matching hits are materialized before stable
  sorting and the public limit. Working memory and search cost can grow with
  corpus/match counts; result limits bound returned data, not collector memory.
- Collection integrity checks compare source documents with managed text fields,
  IDs and the count. They do not prove every native Tantivy posting is intact.
  Owned schema and the native directory are checked on connection open;
  managed/internal storage cannot be accessed through public SQL.
- All seven native clients and SQL/CLI share the parser and frontend. Browser/
  WASM support was removed from the active checkout and is not a release gate.

## Historical native issues and qualification (0.7.2)

Native `ORDER BY score DESC,id LIMIT n` can return arbitrary members of a score
tie because it limits in the FTS method before ID sorting. The frontend avoids
this with an explicitly materialized complete hit set. A 40-document tie fixture
returns the same first three typed IDs as the fully ordered result.

The native shared directory cache can retain a different connection's pager.
The approved fix restricts reuse to the requesting pager before the existing
rollback check. The native regression covers reader isolation, a pinned snapshot
across commit, savepoint rollback and full rollback. Managed/client acceptance
is recorded in [verification](v2-fulltext-evidence.md). The [V2 checklist](v2-tasks.md)
is complete; current operating guidance is in [deployment](deployment.md) and
[operations](operations.md).
