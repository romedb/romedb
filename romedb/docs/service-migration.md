# Move an offline WAL database to the local service

These commands are included in RomeDB 2.3.0. The migration targets Linux x64 and the current
native RomeDB WAL format.

The service requires MVCC, which allows concurrent transactions within one
database-owning process. Migration creates a converted copy in a new directory.
It preserves the source database and creates a separate verified backup before
conversion. Ordinary `serve` startup never converts a database.

## Convert the database

Stop the application that owns `app.romedb` and close all of its database handles.
Choose unused output and backup directories. Their parent directories must exist.
Run the released executable with paths on the local filesystem:

```sh
romedb migrate-to-mvcc app.romedb ./app-service --backup ./app-before-mvcc
romedb serve ./app-service/database.romedb --port 4321
```

The migration requires an owned regular source file with one link. It refuses
symlinks, existing destinations, a live file owner, and unexpected journal files.
The command holds the source file lock until it publishes the converted directory.
Application processes must respect the native database ownership lock.

The backup contains the original main file, its WAL when present, and a
`manifest.json` with file lengths and SHA-256 hashes. Backup and output directories
use mode 0700. Copied files use mode 0600. The command copies committed WAL frames
before it opens or checkpoints the working copy.

The command checks physical integrity, logical metadata, relational row counts,
document values, and scalar, full-text, vector, and spatial indexes. It compares
these checks before conversion, after conversion, and after reopening the MVCC
copy. It checks the backup hashes again before publishing the output.

Default document checks allow 100,000 documents and 64 MiB of encoded document
values across all collections. For a larger database, set explicit audit limits:

```sh
romedb migrate-to-mvcc app.romedb ./app-service --backup ./app-before-mvcc \
  --max-documents 500000 --max-encoded-bytes 268435456
```

These limits bound the document audit. They do not bound the total main-file size
or the work of physical integrity checks. The command uses a fixed copy buffer.
It needs disk space for the raw backup and converted copy, including journals.

The command syncs the copied files and directories. It then publishes the output
directory without replacing an existing path. This keeps the converted main file
and its MVCC journal together. The pinned engine names the MVCC journal
`database.db-log` for `database.romedb`.

After successful conversion, configure both the application and Studio with the
service URL and token. The source remains available as an offline WAL database.
Writes to the converted copy do not update the source or its backup.

## Restore the backup

Choose a new recovery directory with an existing parent. Restore the verified
backup with the same development executable:

```sh
romedb restore-migration ./app-before-mvcc ./app-recovered
```

The restored database is `./app-recovered/database.romedb` in WAL mode. The command
checks the manifest, file hashes, database integrity, and managed indexes before
publishing it. It preserves the backup and refuses an existing recovery directory.
The same document audit flags are available for restore.

The recovery directory is a copy of the database at migration time. It does not
contain later writes to the MVCC service database. Close the service before moving
application ownership back to an embedded database.

## Handle an interrupted or failed migration

The source stays unchanged when copying, auditing, or conversion fails. A failed
command can leave a backup or private staging directory. Do not overwrite these
paths to retry. Use fresh destination names and inspect the retained files.

A backup without a completed verified manifest is partial. `restore-migration`
refuses partial backups and backups whose hashes differ. The original source
remains the recovery path when backup verification did not finish.

If the command stops around the final directory publication, inspect the output
before retrying. A failure while syncing the output's parent can leave a published
directory even though the command reports an error. Keep the source and backup,
check the candidate's integrity, and establish which database owns later writes
before starting an application. Do not infer that an error removed the output.
