import { connect, Client, Float64, Record, Vector, Value, encodeValue, decodeValue, saveRequest, loadRequest, ClientError, RequestLimits, createUploader, UploadError } from '@romedb/node/client';

async function clientTypes() {
  const db: Client = await connect({ url: 'http://127.0.0.1:4321', token: 'test' });
  const limits: RequestLimits = db.limits;
  const batch: number = limits.maxStatements + limits.maxRows + limits.maxRequestBytes + limits.maxResultBytes + limits.maxResponseBytes;
  void batch;
  const result = await db.read([{ sql: 'SELECT $x', params: { $x: new Float64(1) } }], { workLimits: { rowsRead: 100, vmSteps: 1000 } });
  const count: bigint = result.results[0].affected;
  const value: Value = result.results[0].rows[0][0];
  const write = db.prepareWrite([{ sql: 'INSERT INTO posts DOCUMENT $post', params: { $post: { id: new Record('posts', 1n), vector: Vector.float32([1, 2]) } } }], { ifSequence: result.sequence });
  await saveRequest('/private/request.json', write);
  await db.submit(write, { signal: new AbortController().signal });
  await db.recover(await loadRequest('/private/request.json'));
  const engineVersion: string | undefined = (await db.info()).engineVersion;
  const remote = await connect({ url: 'http://127.0.0.1:4321', token: 'test' });
  const file = await remote.exportFile({ signal: new AbortController().signal });
  const exported: number = file.sequence + file.size;
  const reader = file.body.getReader();
  void exported; void reader;
  const session = await remote.files.createUploadSession({ name: 'a.png', size: 10, mediaType: 'image/png', subject: 'user-1', binding: { articleId: 1 } });
  const sessionToken: string = session.token;
  const stored = await remote.files.upload(new Blob(['abc']), { name: 'a.txt', onProgress: step => void step.uploadedBytes });
  const storedId: string = stored.id;
  const page = await remote.files.list({ limit: 10, order: 'desc' });
  const next: string | null = page.next;
  const download = await remote.files.createDownloadUrl(storedId, { disposition: 'inline', expiresInSeconds: 60 });
  const deletion = await remote.files.delete(storedId);
  const pending: boolean = deletion.state === 'pending';
  const charge: number = (await remote.files.usage()).charge.monthlyUsd;
  const job = await remote.files.process(storedId, { kind: 'image', recipe: { width: 320, fit: 'inside', format: 'webp', quality: 80 }, subject: 'user-1' });
  const finished = await remote.files.waitForJob(job.id, { waitMs: 60_000, intervalMs: 500 });
  const made: string | undefined = finished.state === 'ready' ? finished.file?.source?.fileId : finished.error?.code;
  const others: number = (await remote.files.derivatives(storedId)).files.length;
  const canceled: string = (await remote.files.cancelJob(job.id)).state;
  void made; void others; void canceled;
  const film = await remote.files.process(storedId, { kind: 'media', recipe: { output: 'mp4', height: 720, audio: false } });
  void remote.files.process(storedId, { kind: 'media', recipe: { output: 'poster', at: 2.5, width: 640, format: 'webp' } });
  void remote.files.process(storedId, { kind: 'media', recipe: { output: 'audio', format: 'mp3', bitrate: 96 } });
  const filmKind: 'image' | 'media' = film.kind;
  void filmKind;
  // @ts-expect-error a video has no such height
  void remote.files.process(storedId, { kind: 'media', recipe: { output: 'mp4', height: 700 } });
  // @ts-expect-error an image job takes an image recipe
  void remote.files.process(storedId, { kind: 'image', recipe: { output: 'mp4' } });
  // @ts-expect-error a recipe has no such fit
  void remote.files.process(storedId, { kind: 'image', recipe: { fit: 'stretch' } });
  const uploader = createUploader({ authorize: async intent => { void intent.id; return session; } });
  const uploaded = await uploader.upload(new Blob(['abc']), { name: 'a.txt', context: { articleId: 1 }, signal: new AbortController().signal });
  try { await uploader.abort(session); } catch (error) { if (error instanceof UploadError && error.recoverable) void error.code; }
  void sessionToken; void next; void download.url; void pending; void charge; void uploaded.checksum;
  // @ts-expect-error the size of a file never changes
  await remote.files.update(storedId, { size: 1 });
  // @ts-expect-error an upload declaration needs a size
  await remote.files.createUploadSession({ name: 'a.png' });
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
