# RomeDB / RomeQL 0.2.10

Released for Linux x64 on 2026-10-27. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.10).

RomeDB 0.2.10 updates the `romedb cloud` CLI for the new RomeDB Cloud organization keys.

- RomeDB Cloud now issues organization keys that start with `rdbo_`. The CLI accepts them and refuses keys that start with `fdbo_`. Personal keys (`fdbk_`) and database tokens are unchanged.
- The 0.2.9 CLI refuses `rdbo_` keys. Update to 0.2.10 to use new organization keys with `romedb cloud`.

Database files, storage format, query protocol and every embedded API are unchanged from 0.2.9. 0.2.10 opens databases made by 0.2.9, and qualification checks that upgrade.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.10-qualification.json).

The build source is `e44445afabdfc6273e899a6080a8d22db90ecded`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/40471336539) passes. All 22 installed verification groups pass, including the 0.2.9 to 0.2.10 upgrade and restore on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET. The production-profile Rust consumer and the bounded application workload also pass.
