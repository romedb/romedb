# Local files

`romedb serve` has a file API for local development. It takes the same requests as
managed files on RomeDB Cloud, so `db.files` of `@romedb/client` and the browser uploader
of `@romedb/client/uploads` work against a local server without a change. An application
that stores uploads in Cloud can be built and tested on one machine.

It is not a storage service. It has no processing jobs, no limits by plan and no charge,
and it listens only on loopback.

## Use

```sh
romedb serve app.romedb --create
```

```js
import { connect } from '@romedb/client';

const db = await connect({ url: 'http://127.0.0.1:4321/v1/database', token });
const file = await db.files.upload(await fs.openAsBlob('report.pdf'), { name: 'report.pdf', mediaType: 'application/pdf' });
const { url } = await db.files.createDownloadUrl(file.id);
const rows = await db.read([{ sql: 'SELECT id, name, size FROM romedb_files' }]);
```

With `romedb serve --directory DIR`, each database has its own files under its database
URL, `/v1/organizations/{org}/databases/{id}`.

## Where the files are

The bytes are in a folder beside the database:

| Server | Folder |
| --- | --- |
| `romedb serve app.romedb` | `app.romedb.files/` |
| `romedb serve --directory DIR` | `DIR/{database id}.files/` |

Inside it:

- `objects/{file id}` holds the bytes of one stored file.
- `uploads/{upload id}/{part number}` holds the parts of an upload that is still open.

Folders have mode 0700 and files 0600. The folder is made at the first upload. Deleting
a database of a directory also removes its folder.

The record of a file is its row in the collection `romedb_files` of the same database.
There is no second copy of the record. The service writes the row when an upload
completes, as one more write request of the database: the sequence rises by one. The
fields are the ones Cloud writes: `name`, `description`, `tags`, `metadata`, `size`,
`mediaType`, `checksum` (the CRC-64/NVME of the bytes in base64), `generation`, `status`,
`createdBy`, `createdAt`, `updatedAt` and `source`. [Managed collections](managed-collections.md)
says what SQL may do to these rows.

To copy a database with its files, stop the server and copy the database file and the
folder together.

## Routes

All paths are under the database URL. Replies and errors have the shapes of Cloud; an
error is `{"error": "text"}`.

| Request | Credential | Result |
| --- | --- | --- |
| `POST /files/uploads` | service token | 201, an upload session with its `token` and `url` |
| `GET /files/uploads/{id}` | service or session token | the session and `uploadedParts` |
| `POST /files/uploads/{id}/parts` | service or session token | one signed URL for each asked part |
| `PUT /files/uploads/{id}/parts/{n}?expires=...&signature=...` | none, the URL is signed | 200; stores the part |
| `POST /files/uploads/{id}/complete` | service or session token | `{file}` |
| `POST /files/uploads/{id}/abort` | service or session token | the ended session |
| `GET /files?after=&limit=&order=` | service token | `{files, next}`, oldest first unless `order=desc` |
| `GET`, `PATCH`, `DELETE /files/{id}` | service token | the file; the changed file; the deletion |
| `GET /files/{id}/deletion` | service token | the deletion of a file deleted since the server started |
| `POST /files/{id}/download` | service token | `{url, expiresAt}` |
| `POST /files/downloads` | service token | `{downloads}` for 1 to 100 files |
| `GET /files/{id}/content?...&signature=...` | none, the URL is signed | the bytes |
| `GET /v1/database/files/usage` | service token | stored bytes and counts |
| `GET /v1/organizations/{org}/files/usage` | service token | the same for all databases of a directory |

The service token is the token of `romedb serve`. A session token (`rdbu_...`) belongs to
one upload and works only on the four routes of that upload.

A part must have exactly its size: 8 MiB, and the rest of the file for the last part. An
upload completes when every part is stored. When the request names a `crc64nvme` and the
stored bytes have another checksum, the upload is rejected and its bytes are removed.

A download URL points back at this server. It sends the file's media type and a
`content-disposition` header with the file's name. `inline` is allowed only for images,
PDF, plain text, audio and video; every other type downloads as an attachment.

## What differs from Cloud

- **No processing.** `POST /files/{id}/jobs`, `GET` and `DELETE /files/jobs/{id}` and
  `GET /files/{id}/derivatives` return 503 `File processing is unavailable`.
- **No allowance and no charge.** Usage reports the plan `local`, an allowance of
  9,007,199,254,740,991 bytes and a charge of 0. The disk is the only limit on stored
  bytes. Rules that do not depend on a plan stay: 100 open uploads for a database, names
  of 255 bytes, metadata of 16 KiB, a file of at most 4.995 TiB.
- **Upload sessions are kept in memory.** A session ends when the server stops, and the
  parts it had stored are removed at the next start. Start the upload again with a new
  session. A session that ended is forgotten after a day.
- **Bytes without a row are removed at the start.** A file exists while its row does.
  When the server starts it removes every stored file whose row is not in the database,
  for example after the database file was replaced by an older copy.
- **Signed URLs end with the process.** The key that signs part URLs and download URLs
  is made when the server starts. A URL from before a restart gets 403.
- **Deletion is immediate.** `DELETE /files/{id}` removes the row and the bytes before
  it replies, so `state` is always `done`. The server remembers deletions only until it
  stops.
- **A row deleted with SQL loses its bytes later.** The engine records the delete in
  `romedb_file_changes`. The service removes the bytes and the record before it lists
  files or reports usage, and when it starts.
- **One database is its own organization.** With one database, `organizationId` in
  sessions and usage is the database's id. `peakBytes` is the most stored since the
  server started.
- **Loopback only.** The server accepts only requests whose `Host` is this server on
  `127.0.0.1`, `localhost` or `[::1]`, as for every other route.

## What a browser may call

A page of another origin, such as an application on `http://localhost:5173`, may call:

- the status, part URLs, completion and abort of one upload, with that upload's session
  token in the `Authorization` header;
- the signed part URLs, with `PUT`;
- the signed download URLs, with `GET`, also from an `<img>` or a link.

These routes reply to `OPTIONS` with 204 and send `access-control-allow-origin: *`,
`access-control-allow-methods: GET, POST, PUT, OPTIONS` and
`access-control-allow-headers: authorization, content-type`. They refuse a request that
has a cookie.

A browser sends the cookies of a host name to every port of that name. When the
application's page sets cookies on `localhost`, give the file server's URL with
`127.0.0.1` (or the other way round), so that an `<img>` or a link to a download URL has
no cookie.

Every other route refuses a request that has an `Origin` or `Sec-Fetch-Site` header,
with 403. Opening an upload session, listing, changing and deleting files, and asking
for download URLs take the service token, which belongs on a backend and never in a page.

Tests: `romedb/service/tests/files.rs`, `romedb/service/tests/files-sdk.cjs` and the unit
tests in `romedb/service/src/files.rs`.
