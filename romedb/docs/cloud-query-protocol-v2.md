# Cloud query retry contract candidate

This is an unreleased CLI change for the separately developed Cloudflare service.
It does not change embedded SQL or the released cloud endpoint by itself.

- [x] Replace `expectedSequence` with an immutable `afterSequence` retry-window bound.
- [x] Accept committed responses after intervening requests within the 64-position window.
- [x] Version new one-shot read/query journals as version 2; reject old journals without dispatch.
- [x] Keep request IDs, SQL, origin and organization fixed through uncertain-response replay.
- [ ] Deploy with the matching service after reconciling older journals and reservations.

The CLI obtains a known sequence before creating an operation. Independent
operations may use the same position. A returned sequence must be greater than
the lower bound and at most 64 positions later. The server checks retained
receipts before window expiry, so a matching receipt can replay after that point;
an evicted receipt cannot re-execute with its original lower bound.

Never edit a journal's ID or position to retry an uncertain write. Reconcile its
outcome first. Existing version-1 journals require the older service/client for
reconciliation before cutover; there is deliberately no automatic translation.
The service also offers optional `ifSequence` compare-and-set for edits that
depend on an exact position. This API contract does not itself enable concurrent
native execution or imply that cloud MVCC has been deployed.

Validation: `cargo test --locked -p romedb-cli` includes the synthetic HTTP
contract (intervening commits, lost responses, unchanged retries, wrong reply
identity, and old-journal rejection). The release binary runs the same contract
through `python3 romedb/scripts/check-cloud-cli.py target/release/romedb-cli`.
