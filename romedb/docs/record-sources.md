# Record sources in SELECT

Available in RomeDB/RomeQL 2.3.0.

```sql
SELECT title FROM docs:a;
SELECT d.title, d.profile.city FROM docs:a AS d WHERE d.published = true;
SELECT * FROM docs:a;
SELECT d.title, u.name FROM docs:a d LEFT JOIN users:alice u ON d.author = u.id;
SELECT title FROM type::record('docs', $key);
```

A record source is a collection restricted to one complete typed ID before
projections, filters, grouping or joins. A missing record yields zero rows;
`count(*)` over a missing source yields zero. A missing collection is an error.
`SELECT *` returns the same document shape as a collection SELECT. An omitted
alias defaults to the collection name. Aliases and supported nested SELECT/CTE
forms follow the existing collection query rules.

Integer and string keys remain distinct; `docs:2` and ``docs:`2` `` select different
IDs. Backtick names/keys and the full signed 64-bit integer range retain their
existing record-literal meanings. SQL expressions and typed parameters keep
existing semantics. Dynamic sources use `type::record` with literal or bound
table/key values; the key must be a string or int64. Row-dependent source keys
and arbitrary key expressions are outside this addition. Constructor arguments
have a combined 2 MiB tagged-value bound.
The source uses an ID index lookup, including when an
additional WHERE predicate is present. Outer joins restrict their record source
before null extension.

This adds no new write syntax, catalog version or upstream parser change.
