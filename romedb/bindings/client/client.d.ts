import type { Value } from './values';
export { Float64, Record, Vector, Value, PortableValue, encodeValue, decodeValue } from './values';
export interface Statement { sql: string; params?: { [key: string]: Value }; }
/** timeoutMs: 1 to 600,000 (10 minutes); the default is 20,000. A big commit on Cloud can take minutes. */
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
export class ClientError extends Error {
  readonly code: string;
  readonly outcome: Outcome;
  readonly request?: PreparedWrite;
  readonly status?: number;
  readonly sequence?: number;
  /** The server's Retry-After delay in milliseconds, on HTTP error replies that send one. */
  readonly retryAfterMs?: number;
  /** For RDB_TRANSPORT, RDB_PROTOCOL and RDB_CANCELLED without a reply: the fetch or parse error underneath. */
  readonly cause?: unknown;
  constructor(code: string, message: string, outcome: Outcome, details?: { request?: PreparedWrite; status?: number; sequence?: number; retryAfterMs?: number; cause?: unknown });
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
export class RemoteDatabase implements Client {
  private constructor();
  readonly url: string;
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
