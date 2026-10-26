# Embedded V1 query contract matrix

This consolidates the V1 capability table in the parent RomeQL.md against the
current frontend and acceptance tests. It is a current support map, not a claim
of complete SQLite compatibility or every possible composition of SQL clauses.
The pinned ordinary relational route now uses Turso v0.8.1; collection lowering is
additive. See [upgrade compatibility changes](turso-0.8.1.md), including rejection
of trailing UPDATE/DELETE LIMIT. Chronological notes in status.md and contracts.md may describe earlier
states superseded by later implementations.

| V1 requirement | Current behavior | Acceptance evidence |
|---|---|---|
| Ordinary SQL | Delegate ordinary tables, views and SQL statements to the pinned engine, retaining native scalar types and transaction dispositions | sql_compat.rs; persistence.rs |
| Filters and projections | Typed collection fields, qualified nested paths, scalar expressions, parameters and projection aliases; missing paths lower to SQL NULL while doc::has distinguishes presence | select.rs; expressions.rs; standalone.rs |
| Stars and result identity | Collection star produces a document; relational/closed-source stars expand positional columns; duplicate output labels retain separate positions | stars.rs; derived.rs; ctes.rs |
| Joins | Collection/native/derived sources support ON joins, including INNER/LEFT and the documented RIGHT/FULL forms. USING merges named keys; NATURAL requires closed sources with known column names | using.rs; select.rs; using-joins.md |
| Ordering and pagination | Scalar and typed-record ordering, explicit collation, output names/ordinals and LIMIT/OFFSET on supported SELECT forms | select.rs; compounds.rs; scalar_subqueries.rs |
| Direct records | SELECT collection:key returns zero/one documents; typed constructors in expressions return values. UPDATE/UNSET/DELETE/UPSERT targets use complete typed identity | writes.rs; returning.rs; Node record/cardinality regression |
| Document writes | Object and parameterized inserts, SQL column-list inserts, SET/object patches, UNSET, DELETE and ID-based shallow UPSERT; insert/upsert can atomically create missing collections | writes.rs; checks.rs; auto_collection.rs; combined Node document workflow |
| Query-fed writes | INSERT SELECT validates final documents and maintains indexes atomically. Supported CTE and UPDATE FROM sources materialize candidates before mutation | insert_select.rs; with_writes.rs; writes.rs |
| Validation/indexes | Optional required/nullable typed fields and CHECKs, scalar/reference indexes, unique conflicts and catalog lifecycle | checks.rs; catalog.rs; integrity.rs; persistence.rs |
| One-hop references | Explicit record::fetch projections, batched targets and statement snapshot; no implicit recursive expansion | links.rs |
| Functions/vectors | Supported pinned scalar operations, exact vector operations for validated encodings, bounded bundled slugify/normalize | sql_helpers.rs; vectors.rs; bundled.rs |
| Result cardinality | Positional raw rows with all/first/exactlyOne; collection SDK helpers return documented document shapes | returning.rs; bindings/node/test.cjs; node-sdk.md |

Test filenames in this table are under tests/tests/ unless another path is shown.
The [language acceptance review](v1-language-acceptance.md) records inspected
assertions, the combined example workflow and scoped verification results.

## Context-specific limits

These are documented application constraints, not an invitation to build every
speculative combination before release:

- Collections target the main database. TEMP/attached collections are deferred;
  ordinary SQL remains subject to pinned support.
- Collection SELECT does not define generic object/array/vector ordering or
  grouping equality. Use scalar projections and explicit functions. Fetch is a
  projection stage, so fetched documents cannot be predicate/order/group keys.
- NATURAL joins need closed source column lists. Project collection fields in a
  derived source before using NATURAL. A direct open-schema collection has no
  fixed column intersection to infer.
- Direct collection UPDATE FROM accepts inner/cross/comma/LEFT and a leading
  RIGHT join with ON or no constraint. Its direct FROM does not accept USING,
  NATURAL, FULL or non-leading RIGHT joins; a supported derived SELECT can supply
  explicit projected source columns. SELECT support does not imply write-FROM
  support for the same spelling.
- Scalar GROUP BY/HAVING, native-supported windows, CTEs and set operators have
  implementation and differential evidence in grouping.rs, windows.rs, ctes.rs
  and compounds.rs. Pinned custom-window-frame and lag rejections remain errors.
  Correlation, lexical alias collisions and nested compound pagination retain
  context-specific restrictions documented with their regressions; general
  arbitrary correlation is not established by this matrix.
- The V1 release deferred collection ON CONFLICT targets. The 2.2.0 source adds
  ID and declared unique targets under the [2.2.0 contract](v2.2-language.md#collection-on-conflict-source-implemented-release-pending);
  exact release qualification is pending.
  Ordinary relational ON CONFLICT remains native. RETURNING rejects aggregate,
  window and subquery expressions and preserves its documented snapshot rules.
- Search indexes, inverse links, user JavaScript, changefeeds and scripting are
  deferred. Recognized future statement forms return FDB_UNSUPPORTED with their
  planned version; ordinary native syntax failures retain engine diagnostics.

## Values, errors and release use

Node integers are bigint, records retain collection plus string/bigint key, and
binary/vector values are explicit wrappers/bytes. Positional QueryResult rows
preserve duplicate labels; TypeScript generics do not enforce stored schemas.
Portable transfer has its own versioned encoding, separate from internal storage
and CLI query JSON. See [transfer.md](transfer.md) and the value-boundary review.

Inspect error codes and transaction observations before retrying. Validation
failure does not imply every native engine error has the same rollback outcome;
FDB_ROLLBACK is not confirmation that rollback completed. The pending trigger
interrupt exception remains S3 work, not a limitation waived by this document.

All capability categories named in the V1 table have implementation evidence.
The remaining release tasks are the finite S1–S7 rows, including final candidate
checks and distribution. Any new blocker must identify a concrete requirement
or reproduced application failure, rather than reopening generic qualification.
