# S4 upgrade and restore evidence

On 2026-09-14 the preview.2 installed Node package created a database in one
process, the current development SDK opened and updated it in a second process,
and a third process reopened it. All phases passed with Node 24.19.0.

Command from the checkout:

```sh
node romedb/scripts/check-preview-upgrade.cjs \
  /tmp/romedb-preview2-consumer/node_modules/@romedb/node \
  /home/tan/Sites/fastdb/turso/romedb/bindings/node
```

Log: /tmp/romedb-v1-upgrade.log. Current implementation source is c4dd652bc;
the addon was rebuilt by the passing scoped run for that implementation.
Old addon SHA-256: a71a69238676ea1beedd4f67df1bd823af3baf3384e0a7d3c658108645e6d6d6.
Current addon SHA-256: b8f610826ee0fb950dceccc72e849368cda3a161e5baf898f0a90acd86254eb3.

Assertions verify the old indexed document and relational row, logical index
integrity, rollback spanning both models, rejected uniqueness conflicts with no
extra document, automatic creation on UPSERT after upgrade, and persistent
post-upgrade data on reopen. The fixture is removed after successful completion.
This tests the previous published package against a local development addon;
it is not yet the final distributable-artifact check or a downgrade promise.

The same implementation also passed
checkpointed_offline_backup_restores_schema_values_indexes_and_history in
the full scoped run (/tmp/romedb-deferred-scoped.log). Its assertions were
reviewed: successful zero/busy-free truncating checkpoint, all handles closed
before file copy, fresh restore directory, native integrity_check, typed values
including exact floating bits, relational view, validators, managed indexes,
exact migration history, rejected invalid/duplicate writes, rollback and reopen.
It verifies that subsequent original/restored writes leave the backup unchanged.
See [the operational procedure](backup-restore.md).

S4 development-candidate rehearsal is complete. Repeat the upgrade smoke against
the final packaged binary under S6, and repeat affected restore checks if its
storage implementation changes. Do not infer online backup, power-loss safety or
unbounded cross-version/platform compatibility from these fixtures.
