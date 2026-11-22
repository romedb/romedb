# RomeDB / RomeQL 0.2.28

Released for Linux x64 on 2026-11-22. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.28).

RomeDB 0.2.28 makes five changes asked for by Cossack, for applications that keep some data in relational tables and some in documents. The engine is unchanged from 0.2.27.

- **`null` in the parent of an indexed nested field.** With an index on `address.city`, `UPDATE t SET "address" = NULL` and an insert with `address: null` failed with `non-object parent at address`. Now a field that holds `null` is read as missing by the indexes and validators of the fields under it. A nested `SET` such as `SET address.city = $c` on a `null` parent now replaces the parent with an object; before, it failed.
- **`SET` and `UNSET` in one `UPDATE`.** `UPDATE t SET "team" = $t UNSET "bio" WHERE …` is one statement with one affected count. The clauses can come in either order. A path named in both is refused.
- **A relational write with a condition on a document.** `UPDATE` and `DELETE` on a relational table can test documents in `WHERE`, for example `UPDATE members SET name = $n WHERE id = $key AND EXISTS (SELECT 1 FROM member_fields d WHERE d.id = $id AND d."team" = $t)`. The table needs a rowid, and parameters must be named or numbered. A document in `SET` or `RETURNING`, `WITH`, and `UPDATE … FROM` are still refused.
- **Index columns and foreign keys in `INFO FOR TABLE`.** For a relational table, each index now has `columns` (name, direction and collation of each key column), and the table has `foreign_keys`.
- **A graph edge to a relational row.** `CREATE EDGE TABLE likes FROM users TO posts` accepts a relational table with one TEXT or INTEGER primary key, and `users:u1` names its row. `RELATE` requires the row to exist. Read the row behind `g.node` with `record::fetch(g.node)`, or join it with `ON u.id = record::id(g.node)`.

Limits of a relational edge endpoint. `ON DELETE RESTRICT` and `ON DELETE CASCADE` do not apply to it: deleting the row leaves its edges, so delete them in the same write (`DELETE FROM likes WHERE source = users:u1`). A traversal does not start from or return a row that is gone. `DROP TABLE` on the endpoint is refused while an edge table names it. See [the graph guide](graph.md).

Upgrading. 0.2.28 opens databases from 0.2.13 to 0.2.27 with no change. A file that has an edge table with a relational endpoint cannot be opened by 0.2.27 and earlier; other files stay readable by them. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud stays on 0.2.24, so these changes are not on Cloud yet.

Security. The shipping dependency review finds no vulnerabilities. It accepts one informational notice, RUSTSEC-2026-0543: `chrono` is unmaintained. The Turso engine uses it, it has no fixed version, and 0.2.27 ships the same version. See [the review](dependency-security.md) and [qualification](release-0.2.28-qualification.json).

The build source is `e8c4a243c1d31e5529593468224b31dcac371325`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/44991892393) passes. All 52 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 to 0.2.27 packages. The production-profile Rust consumer and the bounded application workload also pass.
