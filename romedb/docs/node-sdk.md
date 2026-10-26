# Node document SDK

Both Database and AsyncDatabase expose `collection<T>(name)`. This is a document
convenience API over the existing embedded connection; raw `execute`, `all`,
profiling, migrations and transfer remain available for SQL and advanced queries.

```ts
import { AsyncDatabase } from '@romedb/node';
const db = await AsyncDatabase.open('app.db');
const people = db.collection<{name: string; active: boolean}>('people');
try {
  const alice = await people.upsert('alice', {name:'Alice', active:true});
  await people.merge(alice.id.key, {active:false});
  console.log(await people.get('alice'));
} finally {
  await db.close();
}
```

| Method | Result and behavior |
|---|---|
| all() | Array of documents, no ordering guarantee; materializes the collection |
| get(key) | Document or undefined for a missing record |
| insert(document) | Created document including typed id; generates an ID when omitted |
| upsert(key, fields) | Creates or shallow-merges a record, returning the resulting document |
| merge(key, fields) | Shallow field update of an existing record; missing record returns undefined |
| delete(key) | Removed document, or undefined if absent |

Insert and upsert automatically create missing collections. Reads, merge and
delete retain missing-collection errors. Merge/upsert fields must omit id; keys
are strings or bigint, preserving their distinct identities. Nested objects are
replaced as field values, not recursively merged. Empty merge reads the existing
record. Existing validation and indexes still apply. Collection methods reject
relational tables; use raw SQL for them.

Async methods take ExecuteOptions as the last argument for cancellation/deadlines.
Each call checks collection metadata then executes a parameterized statement;
options apply to each underlying operation. The metadata check is not a schema
lock: serialize schema changes with collection calls. Multi-statement transactions
still require exclusive connection ownership. Raw execute retains transaction
metadata when the application needs it; convenience methods return documents.
TypeScript generics describe expected shapes and do not validate stored data;
use field declarations and CHECKs for runtime validation.

Design reference: [SurrealDB JavaScript query execution](https://surrealdb.com/docs/reference/javascript/concepts/executing-queries).
Its structured methods alongside raw queries inspired this API. RomeDB does not
claim SDK compatibility, remote sessions, live queries, or SurrealDB's replacement
upsert semantics. This addition follows preview.2; its published assets are unchanged.
