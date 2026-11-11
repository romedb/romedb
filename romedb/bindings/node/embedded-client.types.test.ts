import { openEmbedded, EmbeddedDatabase, Float64, Record, Vector, type Client } from '@romedb/node/embedded-client';
async function fixture(): Promise<void> {
  const database: EmbeddedDatabase = await openEmbedded({ path: '/tmp/app.romedb', create: true, maxOpen: 32 });
  const client: Client = database;
  const statements: number = database.limits.maxStatements;
  void statements;
  const info = await client.info();
  const request = client.prepareWrite([{ sql: 'INSERT INTO docs DOCUMENT $doc', params: { $doc: { id: new Record('docs', 'a'), vector: Vector.float32([1, 2]), float: new Float64(-0), n: 9223372036854775807n } } }], { ifSequence: info.sequence });
  const result = await client.submit(request);
  const affected: bigint = result.results[0].affected;
  void affected;
  const saved: { sequence: number; bytes: number } = await database.exportFile('/tmp/copy.romedb', { signal: new AbortController().signal });
  const streamed = await database.exportFile({ timeoutMs: 60000 });
  const body: ReadableStream<Uint8Array> = streamed.body;
  const size: number = streamed.size;
  void saved; void body; void size;
  // @ts-expect-error exports take only a signal and a timeout
  await database.exportFile({ afterSequence: 1 });
  await client.close();
}
void fixture;
