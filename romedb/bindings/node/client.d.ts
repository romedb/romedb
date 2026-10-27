export class Float64 { constructor(value: number); readonly value: number; }
export class Record { constructor(table: string, key: string | bigint); readonly table: string; readonly key: string | bigint; }
export class Vector {
  constructor(bytes: Uint8Array);
  readonly bytes: Uint8Array;
  static float32(values: readonly number[] | Float32Array | Float64Array): Vector;
  static float64(values: readonly number[] | Float32Array | Float64Array): Vector;
  static sparse32Entries(dimensions: number, entries: readonly (readonly [number, number])[]): Vector;
}
export type Value = null | boolean | bigint | number | string | Float64 | Record | Vector | Uint8Array | Value[] | { [key: string]: Value };
export type PortableValue = { type: 'Null' } | { type: 'Boolean'; value: boolean } | { type: 'Integer' | 'Number' | 'String'; value: string } | { type: 'Binary' | 'Vector'; value: number[] | string } | { type: 'Record'; value: { table: string; key: { type: 'Integer' | 'String'; value: string } } } | { type: 'Array'; value: PortableValue[] } | { type: 'Object'; value: { [key: string]: PortableValue } };
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
export type Outcome = 'not-sent' | 'read-failed' | 'unknown' | 'rolled-back';
export class ClientError extends Error {
  readonly code: string;
  readonly outcome: Outcome;
  readonly request?: PreparedWrite;
  readonly status?: number;
  readonly sequence?: number;
  constructor(code: string, message: string, outcome: Outcome, details?: { request?: PreparedWrite; status?: number; sequence?: number });
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
  /** True when connect() skipped the details request (the default for Cloud URLs). */
  readonly deferred: boolean;
  /** Ask Cloud to start the database's container and open the database without running a query. */
  warm(options?: RequestOptions): Promise<WarmResult>;
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
export function encodeValue(value: Value): PortableValue;
export function decodeValue(value: unknown): Value;
export function normalizeUrl(url: string): string;
export function validatePrepared(request: unknown): { requestId: string; afterSequence: number; ifSequence?: number; statements: unknown[] };
export function saveRequest(path: string, request: PreparedWrite): Promise<void>;
export function loadRequest(path: string): Promise<PreparedWrite>;
