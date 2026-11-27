# Managed collections

RomeDB Cloud keeps one row for each uploaded file inside the customer's database, in the
collection `romedb_files`. The service owns that collection. This page says what the
engine enforces, for a database in Cloud and for a file opened anywhere else.

## Reserved names

A name that starts with `romedb_` is reserved. A statement cannot create a table, a
collection or a view with such a name, cannot rename a table to one, and cannot create a
collection by writing to one. The error is `validation: names that start with romedb_ are
reserved`.

A database made by an earlier release that already has such a table opens and reads as
before. Two names get the rules below whatever made them: `romedb_files` and
`romedb_file_changes`.

## `romedb_files`

A row is a document. The service sets `id`, `size`, `mediaType`, `checksum`,
`generation`, `status`, `createdBy`, `createdAt` and `updatedAt`. The customer owns
`name`, `description`, `tags` and `metadata`.

| Statement | Result |
| --- | --- |
| `SELECT`, joins, record links to a row | Allowed |
| `UPDATE` of `name` (text, 1 to 255 bytes), `description` (text, at most 2,000 bytes), `tags` (at most 32 texts of 1 to 64 bytes), `metadata` (an object, at most 16 KiB) | Allowed |
| `UPDATE` of any other field, or a new field | `validation: romedb_files.<field> is set by the service; ...` |
| `DELETE` | Allowed. It is the request to delete the file |
| `INSERT`, `UPSERT` of a new row, `INSERT OR REPLACE` | `unsupported: rows cannot be inserted: ...` |
| `DROP TABLE`, `DEFINE FIELD`, `DEFINE SCHEMA`, `REMOVE FIELD`, any `CREATE INDEX`, an edge table that ends at it | `unsupported: the definition cannot change: ...` |

A link from another collection to a file row is an ordinary weak link. Deleting the row
leaves the link in place.

## `romedb_file_changes`

Each time a statement changes one of the four fields of a row, or deletes a row, the
engine writes one row here in the same transaction: `{id, file, deleted, version}`. There
is one row for each file. A later change raises `version`; `deleted` stays true once it is
true. A rollback removes the row with the change.

A customer can read this collection and cannot write it. RomeDB Cloud reads it after the
commit, deletes the file or copies the changed fields, and removes the row at the version
it read.

A file that is opened outside Cloud has nobody to read these rows. Deleting a row of
`romedb_files` there removes the row, records the request, and deletes nothing else.

## Host access

A host that embeds the engine marks its own requests:

```rust
connection.set_host_access(true);   // this connection now writes both collections freely
connection.take_file_changes();     // how many customer changes this connection recorded
connection.set_file_access(write, delete); // what a customer credential may do to file rows
```

With host access, statements create and write both collections without the rules above
and record nothing in `romedb_file_changes`. Creating `romedb_files` also creates
`romedb_file_changes`. A host must turn host access on only for statements it wrote
itself.

`set_file_access` narrows customer access: without `write`, an `UPDATE` of a file row is
refused, and without `delete`, a `DELETE` is. Both are allowed until a host says
otherwise, which is the case for `romedb serve` and for embedded use.

## Copies

A file export with `new_identity` becomes another database, so it carries no row of
either collection: the rows name files of the database it was copied from. An export
that keeps its identity keeps them.

Tests: `romedb/tests/tests/file_catalog.rs`.
