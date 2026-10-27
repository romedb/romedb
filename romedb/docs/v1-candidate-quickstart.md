# RomeDB embedded candidate

This optimized Linux x64 bundle is a local release candidate. Check manifest.json
for its exact source, build profile and Node package filename. It is not a stable
release until the remaining requirements in LIMITATIONS.md are closed. Node
22/24 and Rust 1.88.0 are the verified development targets; other platforms and
Linux distributions are not established by this bundle.

From the extracted directory:

```sh
sha256sum -c SHA256SUMS
printf "INSERT INTO notes {id:notes:first,text:'hello'}; SELECT notes:first;\n" | ./romedb-cli notes.db
npm install --offline --ignore-scripts --no-audit --no-fund --no-save ./romedb-node-1.0.0.tgz
node tracker.cjs ./tracker.db
```

For the document SDK, save this as example.cjs and run `node example.cjs`:

```js
const { AsyncDatabase } = require('@romedb/node');
(async () => {
  const db = await AsyncDatabase.open('app.db');
  try {
    const notes = db.collection('notes');
    await notes.upsert('first', {text: 'Hello'});
    console.log(await notes.get('first'));
  } finally { await db.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
```

Insert/upsert create missing collections automatically. Each collection has its
own backing table in the database file. Upsert and merge shallow-patch fields;
reads do not create collections. Raw parameterized SQL remains available through
execute/all/first/exactlyOne. Integers use bigint and document IDs use Record.
TypeScript generic shapes are expectations; declare fields/CHECKs for validation.

See SDK.md, QUERY-CONTRACT.md, BACKUP.md and PERFORMANCE.md in this bundle.
Resolve transactions and stop all access before the documented offline backup.
Inspect error transaction observations before retrying writes. There is no cloud
service or registry-install promise associated with this local artifact.
