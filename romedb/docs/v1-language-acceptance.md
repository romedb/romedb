# S1 language acceptance review

S1 implementation milestone accepted on 2026-09-14 at source 2ca34930b:
the query matrix and value/error review are consolidated, all fourteen acceptance
items have evidence or their explicit final-artifact dependency, and the full
scoped check passed with 677 Rust tests, one known ignored S3 trigger test,
106 Node/application tests, formatting, Clippy and strict TypeScript.
Log: /tmp/romedb-v1-language-final.log. The combined document workflow is included.
Final SDK version/artifact checks remain S6; S3 is not waived by S1 acceptance.

Milestone verification on 2026-09-14: `bash romedb/scripts/check.sh` completed
successfully with 676 Rust tests passed, one known ignored trigger-cancellation
test, 105 Node/application tests passed, formatting, Clippy and strict TypeScript.
Log: /tmp/romedb-v1-acceptance-check.log. Executable source corresponds to
8bd196c5c; the only intervening commit 22fe10260 records the diagnostic gap below.
This includes the new parameter-spelling and record/cardinality tests. It does
not close the deferred-feature diagnostic gap or approve the isolated core patch.

This index follows the fourteen acceptance bullets in the authoritative parent
RomeQL.md, section 10. It identifies existing evidence to inspect, not fourteen
new test campaigns or a claim that every SQL combination is supported. The
stable target remains the full embedded scope in v1-release-contract.md.

Paths below are relative to romedb/. Test files are evidence locations; their
presence alone does not close an acceptance item. The latest recorded scoped
run for c4dd652bc is 677 Rust tests, one known ignored trigger test, and 105 Node
tests; see the deferred-feature repair evidence below. Final-candidate acceptance
remains separate.

| RomeQL acceptance item | Existing evidence location | Review disposition |
|---|---|---|
| 1. Pinned ordinary SQL baseline | UPSTREAM.md; tests/tests/sql_compat.rs | Reviewed: ordinary_sql_matches_the_pinned_engine compares column names and typed rows against direct engine execution, including CRUD, rollback, aliases, CREATE AS SELECT and native ON CONFLICT; malformed_collection_sql_preserves_native_parse_errors_and_active_work compares parse errors and retained transaction state |
| 2. Grammar and parameter ambiguities | parser/src/tests.rs; tests/tests/writes.rs; tests/tests/sql_compat.rs; bindings/node/test.cjs | Reviewed grammar assertions plus focused both-client runtime binding regression; see below |
| 3. Collection versus relational CREATE | tests/tests/catalog.rs; tests/tests/persistence.rs; parser/src/tests.rs | Reviewed: sql_boundaries_and_ordinary_tables verifies native integer primary keys after bare IF NOT EXISTS and CREATE AS SELECT; info_is_logical_and_if_not_exists_never_converts_models asserts the existing document model survives a column-list IF NOT EXISTS; parser tests distinguish the three CREATE routes |
| 4. Record construction and direct targets | tests/tests/writes.rs; bindings/node/test.cjs | Reviewed: fixed/dynamic construction, distinct integer/string keys, wrong-target rejection, extractor types and direct-target versus reference projection; focused both-client regression passed |
| 5. Document examples with setup | bindings/node/test.cjs | RomeQL V1 document examples compose with explicit setup in both clients passes the combined workflow described below |
| 6. Missing/null and nested mutation | tests/tests/expressions.rs; tests/tests/writes.rs; parser/src/tests.rs | Reviewed: document_paths_preserve_null_presence_and_typed_values distinguishes null presence and missing paths, preserves a record through quoted dotted-key/array access, and rejects unsupported path forms; writes assertions preserve literal dotted keys, reject invalid parents/duplicate assignments/ID mutation, and check index removal after UNSET; parser rejects duplicate object keys |
| 7. Index consistency and recovery | tests/tests/persistence.rs; tests/tests/integrity.rs; tests/tests/crash_stress.rs; frontend/src/recovery_io.rs | Reviewed assertions cover insert/patch/delete index lookups, failed unique writes, rollback, reopen and process-kill recovery; crash_stress checks expected documents and absent original/archived index entries after each recovered batch. Reuse S2/S3 evidence; the trigger exception remains separately open |
| 8. Lossless typed values | tests/tests/persistence.rs; tests/tests/transfer.rs; bindings/node/test.cjs | Reviewed: persisted int64/binary/boolean/object round trips, distinct numeric/text record keys, native integer primary keys, portable round trips of all five vector encodings, and Node typed-value/record-identity assertions |
| 9. One-hop links | tests/tests/links.rs | Reviewed: batching, typed identity, relational targets, transaction snapshots and shared byte budget; select_fetches_are_typed_one_hop_projections asserts missing references return null, fetched references remain unexpanded, and nested fetch/predicate/order/write fetch forms reject |
| 10. SQL-shaped write validation | tests/tests/writes.rs; tests/tests/insert_select.rs; tests/tests/checks.rs | Reviewed: every_write_path_checks_evaluated_values_and_preserves_indexes rejects invalid object/column-list/parameter inserts, SET/object patches and UPSERT; multirow_failure_rolls_back_and_checks_use_final_candidate checks cross-field final values; later_conflicts_rollback_copies_indexes_and_keep_outer_transaction checks INSERT SELECT leaves prior work and no failed-copy index entries |
| 11. Field metadata lifecycle | tests/tests/catalog.rs; tests/tests/persistence.rs | Reviewed: field_removal_is_metadata_only_and_incompatible_definitions_fail preserves stored name and unique index after REMOVE FIELD; typed_round_trips_and_definition_build_failure rejects a conflicting definition against existing data and then successfully installs the correct definition |
| 12. Row cardinality and helpers | tests/tests/returning.rs; bindings/node/test.cjs | Reviewed: explicit zero/one/many assertions for all/first/exactlyOne and direct record reads in both clients; existing RETURNING tests cover one, many and empty rows, typed metadata and projection-failure rollback |
| 13. Deferred syntax errors | frontend/src/deferred.rs; tests/tests/deferred.rs | Implemented version-specific FDB_UNSUPPORTED for documented future statement forms; focused regression and full scoped acceptance passed |
| 14. SDK and encoding version review | bindings/node/index.d.ts; docs/node-sdk.md; docs/transfer.md; frontend/src/value.rs | Embedded value boundaries reviewed below; final SDK artifact identity/version remains S6 work; cloud wire protocol follows cloud scope |

## Remaining bounded S1 work

1. Include the completed item 5 workflow at the next scoped milestone; its
   focused run passes and its assertions are mapped below.
2. Reconcile the current SELECT/write support with the V1 capability table:
   filters, projections, joins, ordering, pagination and direct record targets.
   Distinguish native rejection, implemented collection support and an actual
   missing required form. Historical status entries are chronological evidence,
   not the current support matrix.
3. Consolidate the reviewed value encodings and error classifications into the
   current language contract. Item 14's final package version belongs to S6;
   encoding and deferred-error implementation reviews are complete here.

For example, contracts.md restricts USING in collection UPDATE FROM while allowing
supported USING forms in SELECT-derived sources. These are different contexts;
the SELECT oracle in tests/tests/using.rs does not prove direct write-FROM support.
Do not erase the write restriction based on that oracle or infer that every join
context needs expansion merely because SELECT supports more forms.

S1 remains open until these three review actions are complete. This document
does not reopen preview requirements P0–P6.

## Grammar review findings

The parser's sql_is_preserved regression explicitly retains bracket identifiers,
colon-containing strings, contextual aliases, single-quoted aliases and all five
parameter spellings. The native differential test executes quoted/contextual
aliases and colon-containing strings, rather than checking dispatch alone.
standalone_record_constructors_and_parameter_names in tests/tests/writes.rs
compares the pinned rejection of `$name::suffix` with the frontend error. Thus
preserving this token spelling does not imply the pinned engine accepts it.
collection_and_records checks backtick string keys, signed int64 minimum and
overflow rejection. The new Node regression `V1 parameter spellings bind through
native and document routes in both clients` executes all five parameter forms
through native scalar SELECT, collection INSERT RETURNING and collection WHERE.
It checks int64 maximum preservation and independently bound numbered, colon,
at-sign and dollar parameters in one statement. The focused Node 24 run passed
against the existing addon on 2026-09-14; log /tmp/romedb-v1-parameters.log.
No engine rebuild or full scoped rerun was performed for this test-only change
during the active vector measurement; include it at final scoped acceptance.

Items 1, 2, 3, 4, 6, 8, 9, 11 and 12 now have inspected assertions for their explicit checklist
requirements; no additional test matrix is requested for them. This review does not
claim that all possible SQL statements or CREATE variants have been enumerated.

The focused Node 24 test `V1 record targets and row cardinality stay explicit in
both clients` passed on 2026-09-14 (log /tmp/romedb-v1-record-cardinality.log).
It covers missing and existing direct targets, a reference-valued SELECT,
integer/string identity, wrong-target validation without extra rows, and
zero/one/many helper behavior. It also verifies multirow UPDATE RETURNING and
empty DELETE RETURNING. This is focused evidence; a new full scoped run has not
yet been performed for these added Node tests.

## Value boundary review

Three different boundaries must remain distinct:

| Boundary | Current representation and compatibility evidence |
|---|---|
| Internal persisted values | `FDB` plus byte version 1, followed by serde-tagged JSON; value.rs rejects unknown prefixes and validates decoded values. This is internal storage, not the portable transfer protocol. Catalog versioning is separate. |
| Portable transfer | `romedb.documents` header version 1; Integer uses a canonical decimal string, Number uses finite binary64 bits as hex, records retain tagged key kinds, vectors/binary retain bytes. transfer.rs verifies JSON/NDJSON cross-database round trips, unknown-version rejection, signed-zero bits, nested tag-like user objects and all five vector encodings. |
| Embedded Node API | index.d.ts exposes bigint integers, number floating values, Uint8Array binary, Record and Vector instances, nested arrays/objects, and positional QueryResult columns/rows. node-sdk.md defines collection helpers and explicitly limits TypeScript generics to expected shapes. This is an in-process API, not a network wire format. |

The portable transfer tests compare signed-zero bits explicitly; ordinary Rust
float equality alone would not establish that distinction. Native primary keys
are checked as integers in persistence.rs, while the Node record-cardinality
test checks typed document IDs and extracted integer/string keys separately.
No encoding change is needed for these reviewed requirements. Final release
packaging must still bind the SDK API to its advertised package version under S6.

## Reproduced deferred-feature error gap

A Node probe on 2026-09-14 created an empty posts collection and attempted these
master-plan examples individually. None executed successfully:

| Form | Observed code |
|---|---|
| SELECT posts:p1 { title, author.* } | FDB_ENGINE |
| DEFINE RELATION authored_posts ON posts FROM posts.author | FDB_ENGINE |
| CREATE SEARCH INDEX posts_text ON posts (title) USING FULLTEXT | FDB_ENGINE |
| SELECT relation::fetch(id,'x') FROM posts | FDB_UNSUPPORTED |
| LET $x = 1 | FDB_ENGINE |

Item 13 explicitly asks for feature/version errors. Its bounded repair is to
classify recognized deferred grammar at the RomeDB boundary and regression-test
these examples, preserving ordinary SQL/contextual identifier dispatch. Do not
enable the features or reserve arbitrary identifiers to satisfy this requirement.

Repair: the frontend now identifies unquoted leading declaration keywords and
direct-record brace projections before native execution. It returns
FDB_UNSUPPORTED with the planned V2/V3 scope for relation/search/function
declarations, changefeed declarations/inspection/removal and procedural LET/DO.
Function namespace rejection remains the existing FDB_UNSUPPORTED behavior.
The focused deferred.rs regression passed on 2026-09-14, checking nine forms,
an intact active transaction, preserved prior data, and ordinary SQL identifier
and string controls. Log: /tmp/romedb-deferred-test.log. Full scoped verification
completed successfully: 677 Rust tests passed with one known ignored trigger
test, 105 Node/application tests, formatting, Clippy and strict TypeScript.
Log: /tmp/romedb-deferred-scoped.log. This closes the reproduced item 13
diagnostic gap; it does not enable any deferred feature.

## Combined document-example workflow

The Node regression `RomeQL V1 document examples compose with explicit setup in
both clients` passed on 2026-09-14 with Node 24 (log /tmp/romedb-v1-examples.log).
It uses the master-plan users/posts examples with explicit collection setup,
required title/name validation, optional typed fields, a unique name index and
an author-reference index. Both synchronous and worker clients execute the same
workflow. The mappings are:

- Optional fields and document inserts: setup declares every validator needed by
  the fixture before inserting Alice and the referenced post.
- Atomic changes: parameterized nested SET, object patch, array::append and UNSET
  execute together; fetched author assertions check their resulting values and
  actual field absence.
- Queries and links: the author JOIN filters with a parameter and uses explicit
  ordering/pagination; forward fetch returns the expected post/author values.
- ID UPSERT: changing the title retains the omitted published field, proving
  shallow patch behavior. An invalid empty title fails validation.
- Direct targets and transactions: DELETE returns through the ordinary API,
  direct SELECT observes absence, ROLLBACK restores the post, and both collection
  integrity audits pass after the complete workflow.

This closes the combined document-example fixture task. Earlier focused tests
remain the evidence for individual grammar/value branches. The added test has
not yet been included in a new full scoped run; retain the last full-run counts
until that milestone finishes.
