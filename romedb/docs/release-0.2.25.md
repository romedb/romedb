# RomeDB / RomeQL 0.2.25

Released for Linux x64 on 2026-11-15. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.25).

RomeDB 0.2.25 fixes two defects: `@romedb/client` refused large Binary values, and the Cloud CLI reported a confirmed failure as unconfirmed. The engine is unchanged from 0.2.24.

- **Large Binary values in the client.** `@romedb/client` refused a Binary of more than 524,289 bytes in a result, and of more than 1,048,576 characters when it arrived as base64. Those are the sizes of a Vector, not of a Binary. So the client could send a value that it could not read back: a read failed with `RDB_PROTOCOL`; a write that returned such a value committed, was reported as unknown, and `recover()` failed the same way; `submit()` of a saved request with such a parameter threw a `TypeError` before it was sent. A Binary is now as large as its request or response allows. The two sizes still apply to a Vector.
- **Base64 values of several megabytes.** The base64 check used a pattern that overflowed the regular expression stack at about 9 million characters. It is now a length check and a simple pattern. Only canonical base64 passes, as before.
- **Confirmed failures in the Cloud CLI.** A Cloud request that ran and failed, for example on a constraint, was reported by `romedb cloud db query`, `db read` and `db retry` as "Unconfirmed response identity; retry the same journal". The real error was never shown, and a retry returned the same stored failure. These commands now print the error, its code, the outcome (`rolled-back`, or `read-failed` for a read), the request ID and the sequence, and exit nonzero. The interactive shell prints the same and accepts the next statement.

Upgrading. 0.2.25 opens databases from 0.2.13 to 0.2.24 with no change, and does not change the file format. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud stays on 0.2.24. The engine is the same, so this release needs no Cloud deployment.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.25-qualification.json).

The build source is `8d55dc24bf92589dd401d1b69a7d0f4d7fa35681`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/43718828697) passes. All 46 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 to 0.2.24 packages. The production-profile Rust consumer and the bounded application workload also pass.
