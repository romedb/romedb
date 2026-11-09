# RomeDB / RomeQL 0.2.21

Released for Linux x64 on 2026-11-09. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.21).

RomeDB 0.2.21 adds search as you type, field weights and snippets to full-text search.

- **Prefixes.** `hel*` finds documents with any indexed word that starts with `hel`, after the index's analyzer runs, so `HEL*` is the same query and `"hel"*` is the same as `hel*`. A prefix combines like a word: `+fast +hel*`, `hel* -help`, and inside parentheses. It matches at most the 64 indexed words in the most documents, and each scores as if the query named that word. A prefix needs at least 2 characters before the `*` and takes one `*` at the end of a word; other forms reject with `RDB_VALIDATION`. Before 0.2.21 `hel*` looked for the word `hel` and `"hel"*` failed with an engine error. This is one new patch to Turso core, listed in the core exceptions.
- **Field weights.** `CREATE SEARCH INDEX articles_text ON articles(title, body) USING FULLTEXT WITH (weights='title=3')` makes a match in `title` count three times a match in `body`. Keys are the index's paths written with dots, and paths left out weigh 1. Weights are part of the index definition: `IF NOT EXISTS` with other weights rejects, `INFO FOR TABLE` lists them, and `REINDEX`, reopening and file exports keep them. Rust: `Connection::create_fulltext_index_with_weights`.
- **`search::snippet(index, text, query, size)`.** Returns the part of `text` that best shows a match for `query`, as `{ text, match }` parts of at most `size` characters (40 to 1000), cut at whole words. It analyzes the text with the index's tokenizer, so phrases, prefixes and case follow the index, and words in a negative clause are not marked. Runs of whitespace become one space, and a cut start or end shows as `…`. It returns plain text, never markup. Rust: `Connection::snippet_text`.

Upgrading. 0.2.21 opens databases from 0.2.13 to 0.2.20 with no change. A collection with a weighted full-text index moves to catalog version 9, which 0.2.20 and earlier refuse to open; a database without weights stays readable by them. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud runs this release.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.21-qualification.json).

The build source is `d95a58af2eba702ebb67408a800da206e2b7c265`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/42581961780) passes. All 38 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 to 0.2.20 packages. The production-profile Rust consumer and the bounded application workload also pass.
