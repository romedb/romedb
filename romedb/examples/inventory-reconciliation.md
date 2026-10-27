# Inventory reconciliation

Run the companion script against a new database:

```sh
cargo run --locked -p romedb-cli -- inventory.db < romedb/examples/inventory-reconciliation.romeql
```

The example changes P1 from 10 to 12 units and P2 from 5 to 2, records two events
with typed inventory references, clears the adjustment table and commits. The
inventory and events are collections; the adjustment staging table uses ordinary
SQL columns and a primary key. One transaction contains the event inserts and
stock updates.

The staging primary key prevents duplicate adjustments for one SKU, avoiding
plan-dependent selection among joined source matches. The quantity validator
rejects negative final stock. In an application, explicitly roll back the whole
transaction if any step fails; statement rollback alone would leave preceding
event inserts pending. Inspect the error's transaction report because some
native engine errors abort the entire transaction themselves.

Before writing events, a LEFT JOIN counts unmatched adjustment SKUs. A relational
CHECK requires that count to be zero, so unknown SKUs reject the batch with
FDB_CONSTRAINT before stock changes or staging cleanup. An application can also
report the unmatched SKUs to its user. The sample does not implement batch
idempotency, competing inventory
reservations or automatic retries. Replaying a batch without an application
idempotency key could apply its deltas twice. The CLI regression runs the actual
script and checks final stock and event values.

A file-backed regression substitutes negative-stock and unknown-SKU adjustments, checks
rollback after the failing CLI closes, then reruns the corrected transaction and
verifies committed state after reopening. Applications that keep a connection
open must perform their own transaction rollback as described above.
