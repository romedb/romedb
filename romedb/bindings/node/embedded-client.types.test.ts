import { openEmbedded, EmbeddedDatabase, Float64, Record, Vector, type Client } from '@romedb/node/embedded-client';
async function fixture(): Promise<void> {
  const database: EmbeddedDatabase = await openEmbedded({ path: '/tmp/app.romedb', create: true });
  const client: Client = database;
  const info = await client.info();
  const request = client.prepareWrite([{ sql: 'INSERT INTO docs DOCUMENT $doc', params: { $doc: { id: new Record('docs', 'a'), vector: Vector.float32([1, 2]), float: new Float64(-0), n: 9223372036854775807n } } }], { ifSequence: info.sequence });
  const result = await client.submit(request);
  const affected: bigint = result.results[0].affected;
  void affected;
  await client.close();
}
void fixture;
