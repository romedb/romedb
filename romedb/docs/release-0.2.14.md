# RomeDB / RomeQL 0.2.14

Released for Linux x64 on 2026-11-04. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.14).

RomeDB 0.2.14 can leave named tables out of a copy, can start a copy as a new database, and fixes a crash in the Node client.

- **Leave tables out of a copy.** `exportFile` and `serve --directory` copies take `emptyTables`, a list of tables to keep without their rows. The new file has those tables and their indexes, but no row from them. The rows are never read, so they cannot reach the file, its free pages or its logs. Change capture history is left out too, because it can repeat row data. A table with edges must be listed with its endpoints. An unknown name fails with `RDB_VALIDATION`, and no file is made.
- **Start a copy as a new database.** `newIdentity: true` keeps the rows and drops the ID sequence and receipts, so a service adopts the file as a new database. `emptyTables` always does this, because receipts can hold row data.
- **Copies in `romedb serve --directory`.** `POST /v1/organizations/{org}/databases` takes `{ name, from, emptyTables }`, and the copy always gets a new identity. `GET .../file` takes `?empty=a,b&identity=new`.
- **Crash fix.** `serve` no longer sends `connection: close` on file replies. On Node 24, `fetch` fails an assertion and ends the process if the socket closes while a reader is paused. A slow `exportFile` reader against `romedb serve` could hit this in 0.2.13.
- **`Record` helpers.** `Record.toString()` returns `table:key`, and `Record.equals(other)` compares two records.

Upgrading. 0.2.14 opens databases from 0.2.13 with no change. This release adds a new local Turso core patch that lets `VACUUM INTO` skip named tables, listed in [core exceptions](core-exceptions.md).

RomeDB Cloud moved to 0.2.14 on 2026-11-04. From this release `exportFile` works on Cloud URLs: a job builds the file first, so set `timeoutMs` to fit the database, at most 120 seconds. Cloud also imports `.romedb` files through `/imports`, copies a database with `POST .../databases` and `{ name, from }`, and has keys limited to a group of databases. A 0.2.13 client still refuses `exportFile` on Cloud URLs.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.14-qualification.json).

The build source is `6afc3100a83dc6a9a6f40f9f790cc8735208f07e`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/41812435853) passes. All 22 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrade from the 0.2.13 package. The production-profile Rust consumer and the bounded application workload also pass.
