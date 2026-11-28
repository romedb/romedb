# RomeDB / RomeQL 0.2.31

Released for Linux x64 on 2026-11-28. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.31).

RomeDB 0.2.31 adds the client calls for file processing jobs of RomeDB Cloud: a job makes a new file from a stored one. The engine, RomeQL and the storage format are the same as in 0.2.30.

- **Jobs on files.** A Cloud database now has `db.files.process(fileId, job)` in `@romedb/client` and `@romedb/node`. It starts a job of one of four kinds: `image` (resize or convert a picture), `media` (a poster picture, an MP4, or the sound of a video or audio file), `text` (the text of a PDF, an HTML page, a plain file, or a picture) and `ai` (a summary, embeddings, or a transcript).
- **Results are files.** `db.files.job(jobId)` and `db.files.waitForJob(jobId)` give the job's state. A finished job has `file`, a managed file like any other: it has a row in `romedb_files` with a link to its source, and it is deleted with its source. `db.files.derivatives(fileId)` lists the files made from a file, and `db.files.cancelJob(jobId)` stops a job.
- **AI jobs need consent.** An `ai` job sends the content of the file to an AI model, so the request must carry `consent: true`. The types require it. What a model wrote is stored as a separate file with `metadata.generated: true`.
- **The same request is one job.** Asking again for the same source and recipe returns the first job and makes no second file.

Your backend needs a database token with `files:write` to start or cancel a job and `files:read` to read one, or an organization key with `manage`. The recipes, limits and examples are in the [client README](../bindings/client/README.md#files-on-romedb-cloud). Outside Cloud, `db.files` has nothing to call.

Upgrading. 0.2.31 opens databases from 0.2.13 to 0.2.30 with no change, and adds no storage format change, so 0.2.30 can open its files. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud runs file processing jobs since 2026-11-28.

Security. The shipping dependency review finds no vulnerabilities. It accepts one informational notice, RUSTSEC-2026-0543: `chrono` is unmaintained. The Turso engine uses it, it has no fixed version, and 0.2.30 ships the same version. See [the review](dependency-security.md) and [qualification](release-0.2.31-qualification.json).

The build source is `0894472f8ab5cbfcb23edfd4f2ed2fdfe28c134a`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/46103220333) passes. All 58 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 to 0.2.30 packages. The production-profile Rust consumer and the bounded application workload also pass.

Benchmark. The bounded workload in the archive is a second run. The first ran while other work used the same disk and measured 97 and 164 operations a second with 1 and 4 connections. Run again in turn with the 0.2.30 archive on a quiet machine, 0.2.31 measured 187 and 181, and 0.2.30 measured 165 and 162.
