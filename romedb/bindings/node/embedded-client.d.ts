export * from './client';
import type { Client, DatabaseInfo, Statement, RequestOptions, ReadOptions, WriteOptions, BatchResult, PreparedWrite } from './client';
export interface EmbeddedOptions { path: string; create?: boolean; timeoutMs?: number; }
export class EmbeddedDatabase implements Client {
  private constructor();
  readonly path: string;
  readonly url: string;
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
export function openEmbedded(options: EmbeddedOptions): Promise<EmbeddedDatabase>;
