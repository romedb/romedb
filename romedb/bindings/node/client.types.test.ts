import { connect, Client, Float64, Record, Vector, Value, encodeValue, decodeValue, saveRequest, loadRequest, ClientError } from '@romedb/node/client';

async function clientTypes() {
  const db: Client = await connect({ url: 'http://127.0.0.1:4321', token: 'test' });
  const result = await db.read([{ sql: 'SELECT $x', params: { $x: new Float64(1) } }], { workLimits: { rowsRead: 100, vmSteps: 1000 } });
  const count: bigint = result.results[0].affected;
  const value: Value = result.results[0].rows[0][0];
  const write = db.prepareWrite([{ sql: 'INSERT INTO posts DOCUMENT $post', params: { $post: { id: new Record('posts', 1n), vector: Vector.float32([1, 2]) } } }], { ifSequence: result.sequence });
  await saveRequest('/private/request.json', write);
  await db.submit(write, { signal: new AbortController().signal });
  await db.recover(await loadRequest('/private/request.json'));
  const engineVersion: string | undefined = (await db.info()).engineVersion;
  await db.close();
  void count; void value;
  // @ts-expect-error ifSequence is a write condition
  await db.read([{ sql: 'SELECT 1' }], { ifSequence: 1 });
  // @ts-expect-error undefined cannot replace a missing document field
  db.prepareWrite([{ sql: 'SELECT $x', params: { $x: undefined } }]);
  // @ts-expect-error sequence conditions use a safe JS integer, not a SQL bigint
  db.prepareWrite([{ sql: 'SELECT 1' }], { ifSequence: 1n });
}
void clientTypes;
const value: Value = decodeValue(encodeValue(new Float64(-0)));
const sparse: Vector = Vector.sparse32Entries(3, [[0, 1], [2, -1]] as const);
void value; void sparse;
// @ts-expect-error remote Record keys require an explicit string or bigint
new Record('posts', 1);
// @ts-expect-error float values require a number
new Float64(1n);
function errors(error: unknown) {
  if (error instanceof ClientError && error.outcome === 'unknown' && error.request) {
    const body: string = error.request.body;
    void body;
  }
}
void errors;

async function deferredConnection(): Promise<void> {
  const deferred = await connect({ url: 'romedb://cloud.romedb.com/v1/organizations/11111111-1111-4111-8111-111111111111/databases/22222222-2222-4222-8222-222222222222', token: 'token', deferInfo: true });
  const flag: boolean = deferred.deferred;
  const warmed: 'ready' | 'starting' | 'skipped' = (await deferred.warm({ timeoutMs: 1000 })).state;
  void warmed;
  await connect({ url: 'https://cloud.romedb.com/v1/organizations/11111111-1111-4111-8111-111111111111/databases/22222222-2222-4222-8222-222222222222', token: 'token', deferInfo: false, warm: false });
  void flag;
}
void deferredConnection;
