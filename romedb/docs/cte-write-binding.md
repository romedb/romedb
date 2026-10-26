# CTE name binding in collection writes

Collection UPDATE/DELETE candidate selection must preserve the pinned engine's write-context lookup. A CTE named after the exposed write target does not necessarily behave like the same CTE in a SELECT. For example, with target rows 1,2,3, a chained `chosen AS (SELECT n FROM target)` can read the entire physical target even when an outer CTE named `target` contains only 2. An explicit write alias changes which name collides.

The frontend binds those physical references before lowering candidates. It follows derived sources, compound arms and expression subqueries. A nested nonrecursive WITH introduces local declarations: a local CTE with the exposed target name shadows the physical binding. Recursive scopes remain outside this rewrite.

Nested WITH definitions directly inside write CTE definitions are lifted into the enclosing list with distinct internal names. FROM references retain their visible aliases. Definitions and materialization settings are retained once, rather than copied into each reference. This avoids the pinned planner's failed metadata SELECT through nested enclosing-CTE references.

Evidence:

- `with_writes.rs` compares 696 successful native/collection write cases across aliases, projection forms, default/MATERIALIZED/NOT MATERIALIZED definitions, direct/derived/compound/expression/nested-WITH shapes, UPDATE/DELETE rows, affected counts and rollback. Another 24 nested target-alias cases reject in the native oracle and are not collection equivalence evidence.
- A separate 96-case native oracle compares nested and flattened closed-source definitions, including local target-name shadowing.
- The isolated callback test in `functions.rs` compares direct and nested CTE write evaluation counts with native writes.
- Constraint/retry/rollback and both Node client regressions cover related target-CTE and nested membership paths.

This is initial qualification of these shapes. Correlated nested definitions, nested WITH in arbitrary expression/derived positions, recursive scopes, broader evaluation/resource behavior and complete V1 compatibility remain open. The native-rejected target-alias shapes do not establish a collection behavior contract.

Nested local-shadowing recovery also covers missing parameters, uniqueness failures, prior pending writes, corrected retry, integrity audits and rollback for collection-name and explicit-target-alias forms.

Explicit nested/outer CTE column lists have 18 native write comparisons covering quoted case-insensitive qualified lookup, projection forms, materialization modes and index integrity.

Sibling-local shadowing is covered by six native write comparisons: two distinct nested definitions reuse a name while a third sibling retains the outer definition, across all materialization modes.

Six positional-binding comparisons verify numbered and anonymous parameters across earlier/nested CTE definitions and assignments, with each materialization mode.

The three probed nested CTE forms referencing the current write alias reject in the pinned engine. Collection candidate-lowering rejection is reported as Unsupported, with prior transaction work retained, rather than as a storage failure.
