export * from './client';
import type { Client, DatabaseInfo, Statement, RequestOptions, ReadOptions, WriteOptions, BatchResult, PreparedWrite, DatabaseFile, FileExportOptions } from './client';
/** timeoutMs has no default for exports: without it, an export runs until it finishes or signal aborts it. */
export interface ExportOptions extends FileExportOptions {}
export interface ExportedFile { sequence: number; bytes: number; }
export interface EmbeddedOptions { path: string; create?: boolean; timeoutMs?: number; maxOpen?: number; }
export class EmbeddedDatabase implements Client {
  private constructor();
  readonly path: string;
  readonly url: string;
  readonly sequence: number;
  readonly sequenceKnown: boolean;
  readonly databaseId: string;
  info(options?: RequestOptions): Promise<DatabaseInfo>;
  read(statements: Statement[], options?: ReadOptions): Promise<BatchResult>;
  prepareWrite(statements: Statement[], options?: WriteOptions): PreparedWrite;
  submit(request: PreparedWrite, options?: RequestOptions): Promise<BatchResult>;
  recover(request: PreparedWrite, options?: RequestOptions): Promise<BatchResult>;
  writeAtomic(statements: Statement[], options?: WriteOptions): Promise<BatchResult>;
  /**
   * Copy the database into a new private file while reads and writes continue. The destination
   * and its log files must not exist. Writes up to and including `sequence` are in the file.
   */
  exportFile(destination: string, options?: ExportOptions): Promise<ExportedFile>;
  /** Copy the database and stream it; nothing is left on disk once the body ends or is cancelled. */
  exportFile(options?: ExportOptions): Promise<DatabaseFile>;
  close(): Promise<void>;
}
export function openEmbedded(options: EmbeddedOptions): Promise<EmbeddedDatabase>;
