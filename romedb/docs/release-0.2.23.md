# RomeDB / RomeQL 0.2.23

Released for Linux x64 on 2026-11-12. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.23).

RomeDB 0.2.23 adds filters, counts and typo tolerance to full-text search, and raises the per-request limits of `romedb serve` and its clients.

- **Filters before ranking.** `WITH (filters='locale,collection')` declares up to 4 filter paths. `search::text(index, $q, 20, NULL, $filter)` takes the filter as a bound object, such as `{ locale: 'en', collection: ['posts', 'pages'] }`: paths combine with AND and the values of one path with OR. The filter applies inside the search, so `limit` hits come back whenever that many match, and scores stay the same. Filter values are stored whole and are not searched as text. An index with filters moves its collection to catalog version 11.
- **Counts.** `SELECT count FROM search::count(index, $q, max, $filter)` counts the matches up to `max` (1 to 100,000) without reading any document. A count equal to `max` means that many or more.
- **Typo tolerance.** `databse~1` finds "database". `~1` needs a word of at least 4 characters and `~2` one of at least 8. A nearby word scores at half weight, so the exact word ranks first, and `search::snippet` marks the typo matches.
- **Larger requests.** `romedb serve` now takes 8 MiB requests with up to 256 statements and returns up to 10,000 rows per statement, 8 MiB of results and 16 MiB responses. A request over 1 MiB counts as several in admission. `@romedb/client` reads the limits the server reports, checks requests and replies against them and exports them as `client.limits`. It states the result limits it accepts in the `x-romedb-result-limits` header, and a request without that header keeps 1,000 rows and 256 KiB, so older clients are unaffected. RomeDB Cloud already runs 0.2.23 with 256 statements, 10,000 rows and 8 MiB fresh reads.
- Filters and typo words are two new patches to Turso core, listed in the core exceptions.

Upgrading. 0.2.23 opens databases from 0.2.13 to 0.2.22 with no change. A collection with a full-text index that declares `filters` moves to catalog version 11, which 0.2.22 and earlier refuse to open; a database without one stays readable by them. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud runs this release.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.23-qualification.json).

The build source is `0e33d4a2d75f47803ff37fa684a7a0fea7f6e5ec`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/43143774322) passes. All 42 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 to 0.2.22 packages. The production-profile Rust consumer and the bounded application workload also pass.
