import type { Value } from './values';
export { Float64, Record, Vector, Value, PortableValue, encodeValue, decodeValue } from './values';
export interface Statement { sql: string; params?: { [key: string]: Value }; }
/**
 * timeoutMs: 1 to 600,000 (10 minutes); the default is 20,000. A big commit on Cloud can take minutes.
 * A client runs up to 32 requests at once. A request keeps its place until it ends, its signal
 * aborts or its timeoutMs has passed, whichever comes first.
 */
export interface RequestOptions { signal?: AbortSignal; timeoutMs?: number; }
/** What a copy of the database leaves out. */
export interface CopyOptions {
  /**
   * These tables keep their schema and indexes but no rows. Their rows are never read, so
   * they cannot reach the file. Unknown names fail with RDB_VALIDATION. The copy also gets
   * a new identity, because a receipt can hold row data.
   */
  emptyTables?: readonly string[];
  /** Leave out the database ID, the sequence and the receipts: the copy is a new database. */
  newIdentity?: boolean;
}
export interface FileExportOptions extends RequestOptions, CopyOptions {
  /** How long Cloud's job may take to build the file. The default is 3 hours, the longest it runs. */
  waitMs?: number;
}
export interface IndexBuildOptions extends RequestOptions {
  /** How long to wait for the job. The default is 3 hours, longer than a build runs. */
  waitMs?: number;
}
/** A finished index build. `bytes` is the database size with the index. */
export interface IndexBuild { id: string; bytes: number; }
export interface WorkLimits { rowsRead?: number; rowMutations?: number; vmSteps?: number; }
export interface ReadOptions extends RequestOptions { afterSequence?: number; workLimits?: WorkLimits; }
export interface WriteOptions extends ReadOptions { requestId?: string; ifSequence?: number; }
export type PreparedWrite =
  | { readonly version: 1; readonly url: string; readonly databaseId: string; readonly body: string }
  | { readonly version: 2; readonly url: string; readonly databaseId: string; readonly queryVersion: 2 | 3; readonly body: string };
export interface QueryResult { columns: string[]; rows: Value[][]; affected: bigint; }
export interface BatchResult { requestId: string; sequence: number; results: QueryResult[]; readVersion?: 2; executionWork?: unknown; }
export interface Capabilities { provenance: 'negotiated' | 'documented'; [key: string]: unknown; }
/**
 * The request limits the client enforces: the ones the database reported in its details. A
 * Cloud connection that has not read its details uses Cloud's documented values. A local
 * service that reports none keeps the earlier values (64 KiB requests, 32 statements, 1,000
 * rows, 256 KiB of results, 512 KiB responses). Every read and write sends maxRows and
 * maxResultBytes in the x-romedb-result-limits header, so the server never returns more.
 */
export interface RequestLimits {
  readonly maxRequestBytes: number;
  readonly maxStatements: number;
  readonly maxRows: number;
  readonly maxResultBytes: number;
  readonly maxResponseBytes: number;
  /**
   * The log one commit may add, in bytes, when the database reports it. RomeDB Cloud reports
   * 1 MiB, or 256 MiB where big commits are on; a connection that has not read its details
   * uses 1 MiB. The client cannot check it before sending: a write past it fails with RDB_LIMIT.
   */
  readonly maxCommitBytes?: number;
  /** The rows one write may change, when the database reports it: 100,000 on Cloud, or 5,000,000 with big commits. */
  readonly maxCommitRows?: number;
}
export interface DatabaseInfo {
  id: string; sequence: number; capabilities: Capabilities;
  /** RomeDB release of the engine, such as "2.7.0". RomeDB 2.7.0 and later report it. */
  engineVersion?: string;
  [key: string]: unknown;
}
export interface ConnectOptions {
  url: string; token: string; timeoutMs?: number; fetch?: typeof globalThis.fetch;
  /** Cloud database URLs skip the details request at connect by default; the first write without afterSequence fetches it. Pass false to request it at connect. */
  deferInfo?: boolean;
  /** Cloud database URLs send a background warm request at connect by default. Pass false to skip it. */
  warm?: boolean;
}
export interface WarmResult { state: 'ready' | 'starting' | 'skipped'; }
/** A copy of the database at one sequence: a complete .romedb file that opens without a log beside it. */
export interface DatabaseFile {
  /** Every write up to and including this sequence is in the file, and none after it. */
  sequence: number;
  /** File size in bytes; the body fails unless exactly this many arrive. */
  size: number;
  /** Read it to the end or cancel it, so the copy's storage is freed. */
  body: ReadableStream<Uint8Array>;
}
export type Outcome = 'not-sent' | 'read-failed' | 'unknown' | 'rolled-back';
/**
 * The limit behind an RDB_LIMIT the client raised itself, before sending anything.
 * `client-requests`: this client object already runs 32 requests. `client-bytes`: its
 * requests in flight already hold too many bytes. Both pass once earlier requests end, so
 * wait and send again. `request-bytes` and `request-statements`: a saved request is larger
 * than this database accepts; it never passes.
 */
export type ClientLimit = 'client-requests' | 'client-bytes' | 'request-bytes' | 'request-statements';
export class ClientError extends Error {
  readonly code: string;
  readonly outcome: Outcome;
  readonly request?: PreparedWrite;
  readonly status?: number;
  readonly sequence?: number;
  /** The server's Retry-After delay in milliseconds, on HTTP error replies that send one. */
  readonly retryAfterMs?: number;
  /** On an RDB_LIMIT raised by the client with outcome `not-sent`: which limit. A refusal from the server has none. */
  readonly limit?: ClientLimit;
  /** For RDB_TRANSPORT, RDB_PROTOCOL and RDB_CANCELLED without a reply: the fetch or parse error underneath. */
  readonly cause?: unknown;
  constructor(code: string, message: string, outcome: Outcome, details?: { request?: PreparedWrite; status?: number; sequence?: number; retryAfterMs?: number; limit?: ClientLimit; cause?: unknown });
}
export interface Client {
  readonly sequence: number;
  readonly databaseId: string;
  /** Limits in force for this connection; they change when info() reads new ones. */
  readonly limits: RequestLimits;
  info(options?: RequestOptions): Promise<DatabaseInfo>;
  read(statements: Statement[], options?: ReadOptions): Promise<BatchResult>;
  prepareWrite(statements: Statement[], options?: WriteOptions): PreparedWrite;
  submit(request: PreparedWrite, options?: RequestOptions): Promise<BatchResult>;
  recover(request: PreparedWrite, options?: RequestOptions): Promise<BatchResult>;
  writeAtomic(statements: Statement[], options?: WriteOptions): Promise<BatchResult>;
  close(): Promise<void>;
}
/** What a backend declares when it opens an upload session. */
export interface UploadDeclaration {
  /** A UUID. The same id with the same declaration returns the same session. Default: a new one. */
  id?: string;
  name: string;
  /** The exact size in bytes. Storage accepts exactly this many and no more. */
  size: number;
  /** Default: application/octet-stream. */
  mediaType?: string;
  /** Up to 16 KiB of JSON, kept with the file. */
  metadata?: { [key: string]: unknown };
  /** Who uploads, in the application's own terms. Kept as the file's `createdBy`. */
  subject?: string;
  /** What the upload is for, such as { articleId }. Kept with the session. */
  binding?: { [key: string]: unknown };
  /** When given, `mediaType` must be one of these. */
  allowedMediaTypes?: string[];
  /** 60 to 604800 (7 days). Default: 86400. */
  expiresInSeconds?: number;
}
export interface FileDeletion { fileId: string; databaseId: string; state: 'pending' | 'done'; requestedAt: number; finishedAt?: number; }
export interface FileUsage {
  organizationId: string; plan: string;
  /** Stored bytes. A deleted file counts until storage removed its bytes. */
  bytes: number;
  /** Bytes held for open uploads. */
  reservedBytes: number;
  files: number; openUploads: number;
  /** The most the organization may store: stored and reserved bytes together. */
  allowanceBytes: number;
  /** Stored bytes are charged in whole terabytes of 10^12 bytes, rounded up. */
  charge: { terabytes: number; usdPerTerabyteMonth: number; monthlyUsd: number };
}
/** How an image job changes a picture. Sizes are pixels, from 1 to 8,192. */
export interface ImageRecipe {
  width?: number;
  height?: number;
  /** 'inside' (default) never makes the picture larger and never crops; 'cover' and 'contain' give exactly width by height and need both. */
  fit?: 'inside' | 'cover' | 'contain';
  /** Default 'webp'. */
  format?: 'jpeg' | 'png' | 'webp' | 'avif';
  /** 1 to 100, default 80. Not used for 'png'. */
  quality?: number;
}
export interface FileJobRequest {
  kind: 'image';
  recipe: ImageRecipe;
  /** Who asked, in the application's own terms. Kept as the new file's `createdBy`; default: the source's. */
  subject?: string;
}
export interface FileJob {
  id: string;
  databaseId: string;
  /** The source file. */
  fileId: string;
  kind: 'image';
  /** The recipe with every default written out. */
  recipe: ImageRecipe;
  state: 'queued' | 'running' | 'ready' | 'failed' | 'canceled';
  createdAt: number;
  startedAt?: number;
  finishedAt?: number;
  /** On a failed or canceled job. Codes: bad_image, too_many_pixels, timeout, too_large, source_gone, allowance, lost, canceled, failed. */
  error?: { code: string; message: string };
  /** On a ready job: the file it made. */
  file?: ManagedFile;
}
export interface DatabaseFiles {
  /** Opens an upload session for one file. Give the result to the browser's `authorize` callback. */
  createUploadSession(declaration: UploadDeclaration, options?: RequestOptions): Promise<UploadSession>;
  /** Uploads bytes this process holds. In Node.js, `fs.openAsBlob(path)` gives a Blob of a file on disk. */
  upload(source: Blob | ArrayBuffer | ArrayBufferView, options?: Omit<UploadDeclaration, 'name' | 'size'> & Omit<TransferOptions, 'fetch'> & { name?: string; timeoutMs?: number }): Promise<ManagedFile>;
  uploadStatus(uploadId: string, options?: RequestOptions): Promise<UploadStatus>;
  completeUpload(uploadId: string, options?: RequestOptions): Promise<ManagedFile>;
  abortUpload(uploadId: string, options?: RequestOptions): Promise<UploadStatus>;
  get(fileId: string, options?: RequestOptions): Promise<ManagedFile>;
  /** One page, oldest first unless order is 'desc'. Pass `next` as `after` for the following page. */
  list(page?: { after?: string; limit?: number; order?: 'asc' | 'desc' }, options?: RequestOptions): Promise<{ files: ManagedFile[]; next: string | null }>;
  /** Changes the fields an application owns. Size, type and checksum never change. */
  update(fileId: string, changes: { name?: string; description?: string; tags?: string[]; metadata?: { [key: string]: unknown } }, options?: RequestOptions): Promise<ManagedFile>;
  /**
   * A URL that downloads the file from storage until it expires (default 300 seconds, at
   * most 7 days). Anyone who has the URL can use it, and it cannot be taken back: check the
   * end user's permission first. `inline` is allowed only for types that cannot run script.
   */
  createDownloadUrl(fileId: string, download?: { expiresInSeconds?: number; disposition?: 'attachment' | 'inline'; fileName?: string }, options?: RequestOptions): Promise<{ url: string; expiresAt: number }>;
  /** The record is gone when this returns. `state` is 'pending' while storage still removes the bytes. */
  delete(fileId: string, options?: RequestOptions): Promise<FileDeletion>;
  deletionStatus(fileId: string, options?: RequestOptions): Promise<FileDeletion>;
  /**
   * Asks Cloud to make a new file from a stored one, such as a resized image. The same file
   * and recipe is the same job, so asking again costs nothing and may return a job that is
   * already 'ready'. Needs `files:write`. Cloud answers 503 where processing is not on.
   */
  process(fileId: string, job: FileJobRequest, options?: RequestOptions): Promise<FileJob>;
  job(jobId: string, options?: RequestOptions): Promise<FileJob>;
  /** Stops a job that has not finished. Needs `files:write`. */
  cancelJob(jobId: string, options?: RequestOptions): Promise<FileJob>;
  /** The files made from a file, oldest first. */
  derivatives(fileId: string, options?: RequestOptions): Promise<{ files: ManagedFile[] }>;
  /** Waits until the job is 'ready', 'failed' or 'canceled'. Throws RDB_TIMEOUT after waitMs (default 120,000); the job goes on in Cloud. */
  waitForJob(jobId: string, wait?: { waitMs?: number; intervalMs?: number; signal?: AbortSignal; timeoutMs?: number }): Promise<FileJob>;
  /** The organization's stored bytes, allowance and monthly charge. Needs an organization key. */
  usage(options?: RequestOptions): Promise<FileUsage>;
}
export class RemoteDatabase implements Client {
  private constructor();
  readonly url: string;
  /**
   * Managed files of this Cloud database. Needs a token with a file scope (files:read,
   * files:write, files:delete) or an organization key with `manage`. Use it on a server only.
   */
  readonly files: DatabaseFiles;
  /** True for a Cloud database. A loopback organization URL (romedb serve --directory) is not Cloud once info() has run. */
  readonly cloud: boolean;
  /** True once a reply has given the database's current sequence. */
  readonly sequenceKnown: boolean;
  /** True when connect() skipped the details request (the default for Cloud URLs). */
  readonly deferred: boolean;
  /** Ask Cloud to start the database's container and open the database without running a query. */
  warm(options?: RequestOptions): Promise<WarmResult>;
  /**
   * Download the database as a .romedb file. timeoutMs covers each request, until the file starts
   * arriving; signal also stops the download. Cloud builds the file in a job, so a large database
   * can take minutes: this waits for it, up to waitMs, then downloads it. The file stays in Cloud
   * for a day.
   */
  exportFile(options?: FileExportOptions): Promise<DatabaseFile>;
  /**
   * Build an index too large for one commit, on Cloud only. Takes one CREATE [UNIQUE] INDEX or
   * CREATE SEARCH INDEX statement, and waits for the job. Writes get 409 until it ends, and reads
   * carry on. timeoutMs covers each request. If signal aborts, waitMs runs out or the client closes,
   * the build is canceled, so writes start again. A failed build gives RDB_INDEX_BUILD with the reason, and
   * leaves the database as it was.
   */
  buildIndex(sql: string, options?: IndexBuildOptions): Promise<IndexBuild>;
  readonly sequence: number;
  readonly databaseId: string;
  readonly limits: RequestLimits;
  info(options?: RequestOptions): Promise<DatabaseInfo>;
  read(statements: Statement[], options?: ReadOptions): Promise<BatchResult>;
  prepareWrite(statements: Statement[], options?: WriteOptions): PreparedWrite;
  submit(request: PreparedWrite, options?: RequestOptions): Promise<BatchResult>;
  recover(request: PreparedWrite, options?: RequestOptions): Promise<BatchResult>;
  writeAtomic(statements: Statement[], options?: WriteOptions): Promise<BatchResult>;
  close(): Promise<void>;
}
export function connect(options: ConnectOptions): Promise<RemoteDatabase>;
export function normalizeUrl(url: string): string;
export function validatePrepared(request: unknown): { requestId: string; afterSequence: number; ifSequence?: number; statements: unknown[] };
/** Node.js only. Elsewhere it rejects; store the PreparedWrite yourself. */
export function saveRequest(path: string, request: PreparedWrite): Promise<void>;
/** Node.js only. Elsewhere it rejects. */
export function loadRequest(path: string): Promise<PreparedWrite>;
import type { ManagedFile, TransferOptions, UploadSession, UploadStatus } from './uploads';
export type { ManagedFile, TransferOptions, UploadIntent, UploadOptions, UploadProgress, UploadSession, UploadState, UploadStatus, Uploader, UploadErrorCode } from './uploads';
export { createUploader, uploadToSession, UploadError } from './uploads';
