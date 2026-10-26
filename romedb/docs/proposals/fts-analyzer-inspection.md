# Shared FTS analyzer inspection

Status: implemented and focused regressions passed for 2.2.0 (2026-10-02).
Combined frontend and release qualification remain pending.
The user authorized engine updates needed by the 2.2.0 RomeQL roadmap.

## Required behavior and gap

S11 needs inspection of the tokens used by a managed full-text index. The pinned
engine supports default/raw/simple/whitespace/ngram analyzers, including ngram
window options, but its tokenizer registration lives in the private FTS cursor.
No public inspection function exposes those exact tokenizers. Duplicating the
construction in RomeDB would let inspection drift from indexing and queries.

## Additive hook

Extract the existing custom registration into a small sibling module under
`core/index_method/fts/`. Keep registration behavior and FTS storage unchanged;
the cursor calls the extracted function. Export a streaming token visitor which
creates the pinned default manager, applies the same registrations, and visits
text/position/position-length/UTF-8 offsets. Expose borrowed token fields through
a small engine-owned struct rather than exposing Tantivy types.

The visitor accepts a caller error type that can wrap engine errors. RomeDB can
then stop emission at its token/byte budgets and return its existing limit code
without a new engine error classification. The hook does no database access,
thread creation, I/O, writes or unbounded result collection. The caller owns the
input and any collected result. RomeDB additionally caps input bytes and ngram
sizes before calling the hook.

Default registration remains Tantivy's default manager. In the pinned 0.26.2
source, the default long-token filter uses UTF-8 byte length strictly below 40,
not a 40-character limit. Raw/simple/whitespace retain their existing case
behavior. Ngrams retain lowercasing and full substring windows.

## Review and acceptance

- Preserve identical cursor registration and storage/schema formats.
- Verify default, raw, simple, whitespace and ngram tokens, case, punctuation,
  Unicode offsets, long-token boundaries and ngram ranges.
- Verify unknown configurations fail and a visitor can stop exactly at its
  budget without receiving further tokens.
- Compare inspection results with real native indexed queries using the same
  configuration; test reopen and updates through managed integration.
- Commit the hook, its focused regressions and this review separately from the
  frontend language feature; list it in the core exception register.

Retirement condition: upstream exposes an equivalent tokenizer visitor shared
with its real index/query registration, and these regressions pass without the
local extraction. No change to FTS index data format is part of this hook.

## Focused evidence (2026-10-02)

The extracted registration is unchanged, and the cursor and visitor use it.
`fts_analysis_hook` passed all three tests, including real native indexes for all
five analyzers and reopen. Existing `fulltext` passed all eight tests. Core
Clippy passed with warnings denied except two pre-existing lint classes:
`assigning_clones` in `core/vdbe/mod.rs` (two execution-meter assignments) and
`unfulfilled_lint_expectations` in `core/json/cache.rs`. Those unrelated sources
were preserved. No dependency, index storage or schema format changes occur.
