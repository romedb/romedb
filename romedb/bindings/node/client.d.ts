import type { Value } from './values';
export { Float64, Record, Vector, Value, PortableValue, encodeValue, decodeValue } from './values';
export interface Statement { sql: string; params?: { [key: string]: Value }; }
export interface RequestOptions { signal?: AbortSignal; timeoutMs?: number; }
export interface WorkLimits { rowsRead?: number; rowMutations?: number; vmSteps?: number; }
export interface ReadOptions extends RequestOptions { afterSequence?: number; workLimits?: WorkLimits; }
export interface WriteOptions extends ReadOptions { requestId?: string; ifSequence?: number; }
export type PreparedWrite =
  | { readonly version: 1; readonly url: string; readonly databaseId: string; readonly body: string }
  | { readonly version: 2; readonly url: string; readonly databaseId: string; readonly queryVersion: 2 | 3; readonly body: string };
export interface QueryResult { columns: string[]; rows: Value[][]; affected: bigint; }
export interface BatchResult { requestId: string; sequence: number; results: QueryResult[]; readVersion?: 2; executionWork?: unknown; }
export interface Capabilities { provenance: 'negotiated' | 'documented'; [key: string]: unknown; }
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
   * Download the database as a .romedb file (romedb serve and serve --directory; not Cloud yet).
   * timeoutMs covers building the copy, until the file starts arriving; signal also stops the download.
   */
  exportFile(options?: RequestOptions): Promise<DatabaseFile>;
  readonly sequence: number;
  readonly databaseId: string;
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
