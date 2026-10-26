# USING join implementation work

Initial collection USING lowering now supports INNER/LEFT/leading-RIGHT joins, chained merged keys, qualified key references and closed-source star suppression. The implementation builds comparison predicates in normalized operand order and keeps merged-key bindings separate from source columns. Collection stars remain whole documents. Closed-source NATURAL joins now have initial shared-column support; open-schema NATURAL and FULL USING remain rejected. This is not complete join release qualification.

`tests/tests/using.rs` verifies ordinary native tables `a(k,left_value)` and `b(k,right_value)`, with matching key 1 and unmatched keys 2/3:

- INNER and LEFT JOIN USING(k) expand unqualified `*` to `k,left_value,right_value`.
- RIGHT JOIN USING(k) expands it to `left_value,k,right_value`: the retained key belongs to the right source.
- Qualified stars retain both key positions. Qualified key references retain each source's NULL extension.
- Unqualified `k` resolves to the retained side, including the unmatched right row.
- The tested FULL JOIN USING(k) form rejects with the pinned equality-condition error. Do not silently implement a different native behavior.

The implementation now builds merged-column metadata before projection/star expansion, field resolution and join-predicate lowering. Replacing USING with ON alone loses unqualified-name resolution and star suppression. Preserve the pinned planner's normalized equality operand order for affinity/collation, source-specific qualified access, left/right NULL extension and post-join filtering. Chained joins need the previously merged left input, not an arbitrary physical source.

Collection stars still represent whole documents under the existing result contract. Do not turn them into variable-schema field expansion while adding join keys. Closed derived/native stars need the pinned suppression/order rules. Explicit USING names must resolve consistently without discovering optional collection fields by executing queries.

Remaining qualification must extend the implementation tests for multiple keys, chained joins, case/quoted names, duplicate outputs, missing keys, NULLs, native affinity/collation and typed record/binary keys. Cover GROUP/ORDER aliases, execute/profile/EXPLAIN, INSERT SELECT constraints and rollback. NATURAL joins use a case-insensitive intersection of closed source column sets. Direct collections require explicit field projections before participating in NATURAL joins.


The multiple-key/chained oracle now also verifies that USING(t,k) does not reorder the retained table columns. A LEFT JOIN on (k,t) followed by JOIN c USING(k) retains the left key when the earlier right row is unmatched. The pinned `a RIGHT JOIN b USING(k,t) JOIN c USING(k)` star expands to `c,a,k,t,b` for the fixture, reflecting engine join reordering. Do not assume written FROM order when implementing RIGHT-join stars; inspect the pinned planner's join normalization and preserve qualified-star behavior separately.

Mixed ON-query unqualified star ordering now mirrors the pinned leading-RIGHT swap/reverse behavior, with differential read/write tests. Merged USING keys and collection USING syntax now have initial support, as described above.


The collation oracle verifies that RIGHT JOIN USING compares the written right-side key first after normalization. With left NOCASE 'A' and right BINARY 'a', INNER/LEFT USING match while RIGHT USING does not; explicit ON a.k=b.k still matches. Reversing the written sources reverses these USING comparison outcomes. A textual USING-to-ON rewrite with unchanged written operand order is therefore incorrect.


Initial implementation verification covers 24 closed-source read shapes (inner/left/right, optional following USING join, unqualified/qualified stars, merged/qualified keys and arithmetic), direct collection key lookup, normal/profiled results, normalized NOCASE/BINARY comparison order, typed record-key writes with uniqueness failure/rollback/reuse, and both Node clients. Aliases/grouping/window scopes, correlated references, duplicate-key columns, missing/native virtual columns and complete resource/evaluation qualification remain open.

A 27-case multi-key implementation matrix now matches native output names and rows for reordered/case-varied/quoted key lists, INNER/LEFT/RIGHT joins and post-join NULL/merged-key filtering through execute/profile.

Closed-source grouping/alias qualification now includes eight normal/profiled native comparisons for source-column precedence, HAVING aliases, ordinal grouping and ordering. Open-schema alias nuances and broader window/correlated scopes remain open.

Inline/named window merged-key partitions and windowed checked writes now have initial read/profile/failure/retry/rollback coverage. Direct source-free scalar/EXISTS subqueries now resolve an outer merged key in their projection and WHERE expressions. The 24-case differential matrix covers INNER/LEFT/RIGHT joins, direct and closed collection sources, scalar arithmetic/filters, EXISTS and a local FROM column that must shadow the outer key. Nested scalar scopes (including `(SELECT (SELECT k))` with RIGHT JOIN), sourceful outer-name fallback, WITH/compound scopes and inner grouping/broader ordering remain unqualified; this change does not claim general correlated USING support.


Typed direct correlation now has 18 read/profile shapes for record, boolean and binary keys across INNER/LEFT/RIGHT joins and direct/closed collection sources, plus correlated collection insertion, unique-index failure, rollback and retry. Nested/sourceful scopes remain unfinished.


Direct source-free correlation now also qualifies outer merged keys in inner ORDER BY, preserving explicit output aliases (including same-name aliases). Implicit column labels are not treated as explicit aliases during qualification. The expanded 66-shape execute/profile matrix covers bare and aliased ordering, WHERE source-before-alias behavior, independent nested EXISTS and LIMIT 0. General nested/sourceful correlation and grouped scopes remain unfinished.


Typed correlation coverage now includes 90 execute/profile shapes with bare/aliased ordering, LIMIT 0 and OFFSET 1 scalar-NULL behavior, plus ordered correlated insertion and transaction failure/retry checks.


Duplicate public key columns now have a 27-case execute/profile differential matrix for duplicates on either/both derived sources, all three supported join kinds, merged/qualified lookup and star positions. Suppression hides all duplicate public key positions on the suppressed side, matching the pinned baseline. Correlated duplicate-key derived fixtures require separate qualification: the tested native scalar variant rejects with no such column k.


Optional and explicit-null keys now match a native NULL baseline in 18 execute/profile shapes across INNER/LEFT/RIGHT joins, direct/closed sources and NULL filters. A missing key in a closed source rejects on either side without preventing later writes and rollback on that connection. Virtual-source and broader correlation qualification remain open.


Managed-index coverage compares nine join/filter shapes before/after index creation and after updates/deletes with index removal. Rollback restores data and index catalog entries. EXPLAIN confirms docs_k for the filtered inner join; this does not establish indexed lookup for the join predicate itself.


Renamed and chained CTE keys now have 27 execute/profile differential shapes across default/MATERIALIZED/NOT MATERIALIZED sources and INNER/LEFT/RIGHT joins. A merged native key uses its resolved public name instead of an SQL-quoted expression label. Materialization evaluation counts remain separate qualification.


Quoted-key metadata now has 27 execute/profile native comparisons for mixed case, spaces, keywords and explicit aliases. Explicit CTE names follow pinned ASCII lowercase normalization; qualified typed projections retain declared names.


Mixed numeric/text affinity now has 18 execute/profile cases against native ON predicates with unary plus on the document-side operand. Documents have no declared SQL affinity; a typeless native column is not an equivalent TEXT-affinity oracle. RIGHT joins retain normalized operand order.


Mixed-affinity coverage now includes 108 execute/profile shapes before/after managed-index creation and numeric/text post-join filtering. EXPLAIN confirms docs_k for filtered direct-source inner joins; this does not claim indexed join lookup.


Nested scalar gap is broader than merged-key lookup: current direct/closed logical sources fail nested k, a.k and b.k references under all three supported join kinds. Nine pinned native cases pass with exact retained/NULL-extended results. Outer-source propagation through nested planning needs implementation; simply qualifying merged names does not address qualified native-side references.


The previously reported nested source-free gap is now fixed by carrying USING scope metadata into recursive scalar lowering. A 54-case execute/profile matrix covers two/three levels, inner WHERE, merged/qualified keys and all three supported join kinds; local FROM shadowing and a typed wrapper also pass. Already-bound compiler accessors are not rebound. General sourceful/WITH/compound and aggregate/window correlation remain unqualified. Both Node clients verify nested RIGHT USING results.


Typed direct/nested correlation now has 180 execute/profile cases for record/boolean/binary keys with ordering, LIMIT/OFFSET and unmatched right keys. Nested ordered insertion also retains unique-index failure/rollback/retry behavior.


Direct scalar CAST routing retains native affinity; outer-source propagation is applied when nested expression plans require it. A 36-case execute/profile matrix verifies the pinned distinction between direct and nested TEXT-cast comparisons.


Initial closed-source NATURAL lowering derives shared keys from normalized join inputs and routes them through the existing USING binding/star-suppression path. No shared keys become an unconditional join. ON/USING constraints combined with NATURAL reject. A 27-case execute/profile matrix matches native columns/rows for one/multiple/no shared keys, INNER/LEFT/RIGHT joins and qualified/unqualified projections. Open-schema, FULL, broader duplicate/collation/chained/correlation and write qualification remain incomplete.


NATURAL collation coverage now includes 18 execute/profile native comparisons with NOCASE/BINARY in both source orders and INNER/LEFT/RIGHT joins, retaining normalized operand precedence and outer-row behavior.


Chained NATURAL joins now have 18 native execute/profile comparisons for INNER/LEFT/leading-RIGHT followed by INNER/LEFT, covering retained keys, qualified access and star positions.


Typed NATURAL RIGHT JOIN insertion now has record/boolean/binary coverage, including unmatched retained keys, unique-index failure, rollback and retry.


Duplicate-column coverage now compares 54 USING/NATURAL shapes with the pinned engine, including duplicates on either/both sides, qualified/unqualified stars and first-key lookup.


NATURAL-to-USING correlation equivalence now covers 12 execute/profile cases for direct/nested scalars, EXISTS and CAST comparisons across INNER/LEFT/RIGHT.


Grouped NATURAL joins have nine native execute/profile cases for merged keys, GROUP BY ordinals, HAVING aliases and same-name shifted projections.


NATURAL merged-key window partitions have six execute/profile native comparisons for inline ROW_NUMBER and named SUM windows across INNER/LEFT/RIGHT joins.


Empty-source NATURAL joins have 18 execute/profile comparisons with the pinned engine: left, right or both inputs empty, with one shared key or no shared columns, across INNER/LEFT/RIGHT joins. NULL extension and result column labels match native behavior.


Table-reading scalar subqueries now resolve unqualified outer USING/NATURAL keys in projections, WHERE and ORDER BY when local sources have closed table schemas. For example, `SELECT k,(SELECT max(n) FROM nums WHERE n<k) FROM docs a RIGHT JOIN b USING(k)` uses the retained right key for unmatched rows. A local table column named `k` takes precedence. Bindings whose retained outer alias is reused by an inner table are left unchanged; general unqualified fallback through alias collisions remains open. Source-free wrappers retain this behavior through nested scalar levels. The resolver inspects schema metadata without stepping the inner query. Open collection inner sources, derived inner sources, WITH/compound scopes and broader grouped/window scopes still need full lexical-resolution qualification.


Table-backed scalar typed-key coverage adds 180 execute/profile cases (record, boolean and binary), including empty results and pagination, plus a uniqueness-failure/rollback/retry INSERT SELECT. Both Node clients also verify boolean/binary nested reads and unmatched NATURAL/USING RIGHT rows.


HAVING in table-backed scalar queries now resolves outer merged USING/NATURAL keys. Explicit result aliases remain available to HAVING and take precedence over outer fallback; local table columns retain native resolution. The native differential matrix includes grouped predicates, aggregate-only HAVING, alias collisions and local grouped-column controls, both directly and inside a source-free scalar wrapper. The pinned engine rejects the probed `SELECT count(*) FROM nums GROUP BY k` with an outer merged key; this work does not add that form.


Inner JOIN ON predicates in table-backed scalar queries now resolve outer merged USING/NATURAL keys. The added 54 native execute/profile cases cover inner JOIN and LEFT JOIN, local-column shadowing, direct/nested scalars and all three supported outer join directions. This uses the same closed-local-schema boundary as scalar WHERE resolution.


Correlated EXISTS/NOT EXISTS and IN/NOT IN now have 72 native execute/profile comparisons covering NULL outer keys, NULL-containing RHS sets, empty filters/LIMIT 0 and aggregate HAVING across USING/NATURAL join directions.


Closed derived SELECT sources are now included in inner correlated-key lookup. Named and unnamed native derived sources expose their output names for local-column shadowing; explicit derived aliases also participate in native qualified-reference scope filtering. The added 72 execute/profile comparisons cover outer merged/qualified keys and local same-name columns through direct/nested scalar wrappers, USING/NATURAL joins and each supported outer join direction. Broader logical derived sources, WITH/compound enclosing scopes, alias collisions and full correlation remain unqualified.


Inner native derived scalar affinity/collation coverage includes 72 execute/profile comparisons for TEXT/NOCASE, unary plus, numeric CAST and RTRIM through ordered/paginated direct and nested scalars.


Collection-backed inner derived sources now have 108 typed execute/profile cases: local-to-outer merged-key equality for records, booleans and binary values, direct/nested scalars, all USING join directions and empty LIMIT/OFFSET results. Broader logical derived scope qualification remains open.


A 12-case native oracle verifies consistent inner alias renaming through direct and nested scalar/EXISTS references across join directions. The corresponding collection merged-key alias collision remains an implementation gap; a rewrite must respect deeper local alias shadowing.
