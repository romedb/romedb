# Previous-release database

`turso-072-fts.db` was created with the published Linux x64 FastDB 2.1.0 CLI
(the project's name before RomeDB), which embeds Turso 0.7.2. It contains a managed FTS index in its original format,
one collection document and one relational account row.

- Archive: https://github.com/romedb/romedb/releases/download/fastdb-v2.1.0/fastdb-2.1.0-linux-x64.tar.gz
- Archive SHA-256: `94d8988ff82cb1649edf4065bd447f11e78ad26a5e558587b3736444ffd2e686`
- Original fixture SHA-256: `df9e34128ec70a4468492728115ca1437d1f67e7ccb27c2bc2c29dee8138f9b1`
- Renamed fixture SHA-256: `3c5f442055402e888e2394a8283f5d562710018165b0abbfbe011dd88365d7ea`

For the RomeDB rename the file was patched byte for byte: `fastdb` became
`romedb` (same length), so page layout, Turso 0.7.2 FTS format and every
B-tree are unchanged. Only object names differ (`__romedb_*`).

```sql
CREATE TABLE articles;
INSERT INTO articles {id:articles:old,title:'legacy searchable text'};
CREATE SEARCH INDEX articles_text ON articles(title) USING FULLTEXT;
CREATE TABLE accounts(id INTEGER PRIMARY KEY,balance INTEGER);
INSERT INTO accounts VALUES(1,100);
PRAGMA wal_checkpoint(TRUNCATE);
```

The upgrade tests copy this fixture to a temporary directory. Never update it
using the current engine: that would erase the previous-release control.
