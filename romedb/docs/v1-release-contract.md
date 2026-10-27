> Historical V1 acceptance contract. V1 is released; the [release record](release-1.0.0.md) supersedes pending/publication language below. Active work is [V2](v2-tasks.md).

# Embedded V1 release contract and remaining work

This is the stable-release target, not a claim that V1 is complete. The parent
RomeDB.md and RomeQL.md remain authoritative. Preview.2 is published; newer source
adds the collection SDK. Cloud and the TypeScript cloud client follow embedded V1.

## Product behavior

| Area | Contract | Implementation reference |
|---|---|---|
| SQL | Ordinary relational queries retain the supported dialect of pinned Turso v0.7.2, SHA 046e9cbf67d22491e8ecc941ec2891b02a9f3cad; document syntax is additive | [Provenance](../UPSTREAM.md), [detailed contracts](contracts.md) |
| Collections | Bare CREATE TABLE creates a collection. Document INSERT/UPSERT can create missing targets atomically. Each collection has its own backing table in the database file | [Contracts](contracts.md#automatic-collection-creation-on-document-writes) |
| Documents and indexes | Nested typed values, typed string/integer record IDs, CRUD, optional field/CHECK validation and scalar/unique indexes; failed writes preserve data/index agreement | [Contracts](contracts.md), [storage evidence](preview-storage-evidence.md) |
| Values | Preserve null, boolean, int64, float64, text, binary, arrays, objects, record and supported vector values; Node uses bigint for lossless integers | [Node declarations](../bindings/node/index.d.ts), [contracts](contracts.md) |
| Transactions | Documents, ordinary tables, metadata and managed indexes share transactions. Errors expose transaction state; callers must inspect it before retrying | [Contracts](contracts.md), [SDK guide](node-sdk.md) |
| Links | Explicit one-hop forward fetch; no inverse traversal or automatic creation of referenced collections | [Contracts](contracts.md) |
| Functions | Pinned supported SQL scalar functions, exact vectors and bounded bundled slugify/normalize; no user JavaScript or external I/O in bundled functions | [Contracts](contracts.md), [benchmarks](benchmarks.md) |
| Tools | CLI, query plans and schema/index inspection, JSON/NDJSON document transfer, forward migrations and backup/restore procedure | [Transfer](transfer.md), [backup](backup-restore.md), [quickstart](preview-quickstart.md) |
| Clients | Rust embedded API and synchronous/asynchronous Node/TypeScript clients, with document helpers alongside parameterized SQL | [Rust guide](rust-client.md), [SDK guide](node-sdk.md) |

No complete SQLite compatibility, automatic schema migration or unmeasured
performance guarantee is promised. SDK generics do not validate stored values.
Nested object updates follow the documented shallow-merge behavior. Query results
currently materialize; configured limits are not a total process-memory cap.

## Finite stable-release requirements

Each row is one required evidence area from the master plan. Existing passing
evidence is reused when source and configuration match. A test failure creates a
specific repair task within its row; it does not add a new qualification program.

| ID | Required outcome | Evidence already available | Remaining concrete work |
|---|---|---|---|
| S1 | Pinned dialect and value semantics | Differential SQL suites; typed value, alias, expression and serialization regressions | [Query matrix](v1-query-matrix.md) and [acceptance review](v1-language-acceptance.md) accepted with the passing scoped run including the combined example workflow |
| S2 | Document/index consistency | Integrated scoped suite passed with 678 Rust tests, zero ignored, and 106 Node tests; integrity audits and failed-write regressions | Include that scoped check on the final implementation candidate; repair any actual candidate regression |
| S3 | Transaction and recovery behavior | Bounded process-kill tests, interrupted statement checks and explicit transaction reports | [Selected commit/checkpoint I/O evidence passed](recovery-io-evidence.md); [approved trigger fix integrated and verified](trigger-interrupt-review.md); include it in final artifacts |
| S4 | Backup and upgrade | Offline restore procedure; released preview.1 to preview.2 upgrade rehearsal | [Preview.2-to-current and offline restore passed](v1-upgrade-restore-evidence.md); repeat upgrade smoke against the final packaged artifact under S6 |
| S5 | Execution/resource controls | Input, candidate/result and deadline controls; cancellation and lifecycle tests | [Scope reconciled](v1-resource-scope.md): retain existing bounded-control tests at final acceptance; streaming and a global allocator cap are not explicit V1 requirements |
| S6 | Installable clients/tools | Published Linux x64 preview bundles; exact Node 22/24 package and TypeScript checks; standalone Rust evidence | Produce final candidate artifacts and notices, identify advertised platforms, test those artifacts, then publish |
| S7 | Document/vector performance evidence | Retained benchmark reports include document/index and 100k vector measurements | [Complete for the measured execution path](v1-million-vector-evidence.md): retained 100k and 1m/768-dimensional reports include latency, RSS, counters and plans; publish the documented capacity limits |

S7's missing measurement is now complete; see the
[million-vector evidence and practical limits](v1-million-vector-evidence.md).
S4's current implementation rehearsal also passed; see
[upgrade and restore evidence](v1-upgrade-restore-evidence.md). Final packaged
binary verification remains part of S6.
S1 and S3 implementation milestones are accepted. Final artifact checks and publication remain open under S6.
The [S1 acceptance index](v1-language-acceptance.md) maps the fourteen RomeQL
acceptance bullets to existing evidence and three bounded review actions.
This document does not silently waive their gaps. The historical [gate review](v1-gates.md)
contains detail; generic statements there about broader qualification are not
instructions to expand these rows indefinitely.

External developer/pilot validation remains a master-plan business workstream.
The user requested SDK development instead of integrating an existing application;
no application repository is awaited. Do not send outreach without authorization.
