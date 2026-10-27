# Metered RomeQL update fix

The result-limit guard must normalize RomeQL update syntax before it sends the
statement to the stock SQL parser. The checked write still executes the original
statement through the existing RomeDB lowering and validation path.

Without this fix, a bounded `UPDATE docs:a SET title='changed'` can fail with
`FDB_VALIDATION` and a parser error before it reaches the intended write. Cloud
native integration reproduced this failure against the released engine pin.

The regression covers WAL and MVCC connections with a required field and unique
index. It verifies a successful record update, constraint rollback, a metered
mutation-limit rejection, unchanged indexed data, and collection integrity.
The low-level meter reports a quota interrupt as `FDB_CANCELLED` with
`mutation_budget_exhausted`; request adapters use that explicit evidence to
report `FDB_LIMIT`. A normal cancellation must retain its own code.

This is a RomeDB frontend change. It does not patch upstream engine sources or
change the released 2.2.0 artifacts.

Metered vector EXPLAIN now returns a measured FDB_UNSUPPORTED rejection before ANN candidate execution. It must not fall through to unmetered execution or produce an unacknowledged Cloud read. Ordinary unmetered EXPLAIN compatibility is unchanged. Focused regression checks both EXPLAIN modes, zero customer reads/mutations, and preserved outer transaction.
