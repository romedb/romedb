import { AsyncDatabase, Database, Record, Vector, Value, isRomeDBError } from './index';
const db = new Database();
const row: Value[] = db.exactlyOne('SELECT $id', { $id: new Record('docs', 1n) });
const rows: Value[][] = db.all('SELECT $blob', { $blob: new Uint8Array([1,2]) });
const changed: bigint = db.execute('SELECT $vector', { $vector: new Vector(new Uint8Array(8)) }).affected;
void row; void rows; void changed;
// @ts-expect-error record keys require explicit string or bigint
new Record('docs', 1);
// @ts-expect-error undefined is not a database value
db.execute('SELECT $x', { $x: undefined });
const migration = db.migrate([{version: 1n, name: 'create', sql: 'CREATE TABLE docs;'}]);
const versions: bigint[] = migration.applied;
const payload: string = db.exportDocuments('docs', 'ndjson');
const count: number = db.importDocuments('docs', payload).imported;
void versions; void count;
// @ts-expect-error versions require bigint
db.migrate([{version: 1, name: 'create', sql: ''}]);
// @ts-expect-error no implicit CSV format
db.exportDocuments('docs', 'csv');

async function checkAsync() {
  const asyncDb = await AsyncDatabase.open();
  const result: Value[][] = await asyncDb.all('SELECT 1');
  const exported: string = await asyncDb.exportDocuments('docs');
  void result; void exported;
  await asyncDb.close();
}
void checkAsync;
// @ts-expect-error async construction must wait for native opening
new AsyncDatabase();
async function checkInterrupt() {
  const asyncDb = await AsyncDatabase.open();
  const live: boolean = asyncDb.interrupt();
  void live;
  await asyncDb.close();
}
void checkInterrupt;
const entries = db.executeBatch('SELECT 1;');
for (const entry of entries) {
  if (entry.result) { const affected: bigint = entry.result.affected; void affected; }
  else { const code: string = entry.error.code; void code; }
}
db.close();

const profileRows: Value[][] = db.profileSelect('SELECT $x', {$x: 1n}).result.rows;
const physicalReads: bigint = db.profileSelect('SELECT 1').metrics.rowsRead;
async function profileAsync(db: AsyncDatabase) {
  const profile = await db.profileSelect('SELECT $x', {$x: 1n});
  const instructions: bigint = profile.metrics.vmSteps;
  const state: 'autocommit' | 'active' = profile.result.transaction.after;
  void instructions; void state;
}
void profileRows; void physicalReads; void profileAsync;

const auditCount: bigint = db.checkCollectionIntegrity('docs', {maxDocuments: 100n}).documents;
// @ts-expect-error integrity limits are explicit bigint values
db.checkCollectionIntegrity('docs', {maxDocuments: 100});
async function auditAsync(db: AsyncDatabase) {
  const bytes: bigint = (await db.checkCollectionIntegrity('docs')).encodedBytes;
  void bytes;
}
void auditCount; void auditAsync;
const factories: Vector[] = [Vector.float32([1,0,-1] as const), Vector.float64(new Float64Array([1])), Vector.sparse32(new Float32Array([1])), Vector.quantized8([1]), Vector.bit1([1])];
void factories;
// @ts-expect-error vector components require numbers
Vector.float32([1n]);
// @ts-expect-error encoded bytes use the Vector constructor
Vector.sparse32(new Uint8Array([1]));
const sparseEntries: readonly import('./index').SparseVectorEntry[] = [[0, 1], [2, -1]] as const;
const sparseFromEntries: Vector = Vector.sparse32Entries(3, sparseEntries);
// @ts-expect-error Entries require index/value tuples.
Vector.sparse32Entries(3, [1, 2]);
// @ts-expect-error Entry components are numbers.
Vector.sparse32Entries(3, [[0, 1n]]);

async function cancellationTypes(db: import('./index').AsyncDatabase) {
  const controller = new AbortController();
  const options: import('./index').ExecuteOptions = {signal: controller.signal};
  await db.execute('SELECT 1', {}, options);
  await db.executeBatch('SELECT 1;', options);
  await db.migrate([], options);
  // @ts-expect-error migration signals must be AbortSignal
  await db.migrate([], {signal: true});
  await db.importDocuments('docs', '', 'json', options);
  await db.exportDocuments('docs', 'ndjson', options);
  // @ts-expect-error transfer signals must be AbortSignal
  await db.exportDocuments('docs', 'json', {signal: true});
  // @ts-expect-error batch signals must be AbortSignal
  await db.executeBatch('SELECT 1;', {signal: true});
  await db.profileSelect('SELECT 1', {}, options);
  await db.checkCollectionIntegrity('docs', {}, options);
  await db.all('SELECT 1', {}, options);
  await db.first('SELECT 1', {}, options);
  await db.exactlyOne('SELECT 1', {}, options);
  // @ts-expect-error signal must be an AbortSignal
  await db.execute('SELECT 1', {}, {signal: true});
}
void cancellationTypes;

const fetchMetrics = db.profileSelect('SELECT 1').metrics;
const fetchCounters: bigint[] = [fetchMetrics.fetchBatches, fetchMetrics.fetchRowsRead, fetchMetrics.fetchVmSteps];
void fetchCounters;


function checkError(value: unknown) {
  if (isRomeDBError(value)) {
    const code: string = value.code;
    const transaction: import('./index').Transaction | undefined = value.transaction;
    const error: import('./index').RomeDBError = value;
    void code; void transaction; void error;
  } else {
    // @ts-expect-error unknown errors do not expose database fields
    value.code;
  }
}
void checkError;

function boundedSelectTypes(sync: Database, asyncDb: AsyncDatabase) {
  const limits: import('./index').ResultLimits = {maxRows: 10n, maxPayloadBytes: 1024n};
  const result: import('./index').QueryResult = sync.selectWithLimits('SELECT 1', limits);
  const profile: import('./index').ProfiledQuery = sync.profileSelectWithLimits('SELECT $x', limits, {$x:1n});
  const pending: Promise<import('./index').QueryResult> = asyncDb.selectWithLimits('SELECT 1', limits, {}, {signal:new AbortController().signal});
  const pendingProfile: Promise<import('./index').ProfiledQuery> = asyncDb.profileSelectWithLimits('SELECT 1', limits);
  // @ts-expect-error both limits are required
  sync.selectWithLimits('SELECT 1', {maxRows:1n});
  // @ts-expect-error limits use bigint
  sync.selectWithLimits('SELECT 1', {maxRows:1,maxPayloadBytes:100n});
  void result; void profile; void pending; void pendingProfile;
}
void boundedSelectTypes;

function writeResultTypes(sync: Database, worker: AsyncDatabase) {
  const limits: import('./index').ResultLimits = {maxRows:1n,maxPayloadBytes:100n};
  const result: import('./index').QueryResult = sync.writeWithResultLimits('INSERT INTO docs {n:$n} RETURNING n',limits,{$n:1n});
  const pending: Promise<import('./index').QueryResult> = worker.writeWithResultLimits('DELETE FROM docs',limits,{}, {signal:new AbortController().signal});
  // @ts-expect-error both budgets are required
  sync.writeWithResultLimits('DELETE FROM docs',{maxRows:0n});
  // @ts-expect-error async cancellation requires AbortSignal
  worker.writeWithResultLimits('DELETE FROM docs',limits,{}, {signal:true});
  void result; void pending;
}
void writeResultTypes;

const buffered = new Database(':memory:', { writeBufferLimits: { maxRows: 10n, maxPayloadBytes: 1000n } });
void AsyncDatabase.open(':memory:', { writeBufferLimits: { maxRows: 10n, maxPayloadBytes: 1000n } });
buffered.close();
// @ts-expect-error buffer budgets require bigint
new Database(':memory:', { writeBufferLimits: { maxRows: 10, maxPayloadBytes: 1000n } });
// @ts-expect-error connection options reject unknown fields
AsyncDatabase.open(':memory:', { unknown: true });
void AsyncDatabase.open().then(async db => {
  await db.execute('SELECT 1', {}, { timeoutMs: 100, signal: new AbortController().signal });
  // @ts-expect-error timeout uses integer milliseconds as a number
  await db.execute('SELECT 1', {}, { timeoutMs: 100n });
  await db.close();
});

function migrationErrorDetails(error: unknown) {
  if (isRomeDBError(error) && error.migration) {
    const version: bigint = error.migration.version;
    const offset: bigint = error.migration.offset;
    const code: string = error.migration.cause.code;
    // @ts-expect-error migration versions are lossless bigints
    const numericVersion: number = error.migration.version;
    void version; void offset; void code; void numericVersion;
  }
}
void migrationErrorDetails;

interface Person { name: string; active: boolean; }
const people = db.collection<Person>('people');
const insertedPerson = people.insert({name:'Alice', active:true});
const personId: Record = insertedPerson.id;
const foundPerson: string | undefined = people.get('alice')?.name;
people.merge('alice', {active:false});
// @ts-expect-error name is a string
people.insert({name:123, active:true});
// @ts-expect-error record keys are string or bigint
people.get(1);
async function collectionTypes(client: AsyncDatabase) {
  const collection = client.collection<Person>('people');
  const result = await collection.upsert('alice', {name:'Alice', active:true}, {timeoutMs:1000});
  const name: string = result.name;
  const removed = await collection.delete(result.id.key);
  void name; void removed;
}
void personId; void foundPerson; void collectionTypes;
