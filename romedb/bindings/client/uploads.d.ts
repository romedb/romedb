/**
 * Uploads to RomeDB Cloud managed files, for browsers, Workers and Node.js. Nothing here
 * takes a database token: a backend opens an upload session with
 * `db.files.createUploadSession()` and the page uploads with that session.
 */

/** A file in RomeDB Cloud. `id` is what an application stores in its own records. */
export interface ManagedFile {
  id: string;
  databaseId: string;
  status: 'ready';
  name: string;
  description: string;
  tags: string[];
  metadata: { [key: string]: unknown };
  /** Bytes, as storage reports them. */
  size: number;
  mediaType: string;
  /** CRC-64/NVME of the whole file, in base64, computed by storage. */
  checksum: { algorithm: 'crc64nvme'; value: string } | null;
  /** Changes when the stored bytes do; a file's bytes never change, so this is fixed for a file id. */
  generation: string;
  /** The `subject` the backend gave when it opened the upload session, or null. */
  createdBy: string | null;
  /** Unix milliseconds. */
  createdAt: number;
  updatedAt: number;
  /** Only on a file a processing job made: the file it was made from, and how. */
  source?: { fileId: string; kind: 'image' | 'media'; recipe: { [key: string]: unknown } };
}

export type UploadState = 'pending' | 'uploading' | 'verifying' | 'ready' | 'aborted' | 'expired' | 'rejected';

/** What Cloud knows about one upload. */
export interface UploadStatus {
  id: string;
  fileId: string;
  databaseId: string;
  organizationId: string;
  state: UploadState;
  name: string;
  size: number;
  mediaType: string;
  /** Every part has this many bytes, except the last. Cloud chooses it. */
  partSize: number;
  parts: number;
  createdAt: number;
  /** Unix milliseconds. After this the session takes no more parts and cannot be completed. */
  expiresAt: number;
  /** Why the upload ended, for aborted, expired and rejected uploads. */
  reason?: string;
  /** Where the session's requests go. */
  url: string;
  /** The part numbers storage holds in full, while the upload is open. */
  uploadedParts?: number[];
  /** The file, once the state is 'ready'. */
  file?: ManagedFile | null;
}

/**
 * Permission to upload one file. `token` works for this upload only and stops with it.
 * Treat it as a secret of the page: do not log it or keep it in persistent browser storage.
 */
export interface UploadSession extends UploadStatus { token: string; }

/** What the page asks its backend to authorize. Everything in it comes from the browser, so the backend checks it. */
export interface UploadIntent {
  /** Pass this on as `id` to createUploadSession, so a later upload() with the same id continues this one. */
  id: string;
  name: string;
  size: number;
  type: string;
  context?: unknown;
}

export interface UploadProgress { uploadedBytes: number; totalBytes: number; uploadedParts: number; totalParts: number; }

export interface TransferOptions {
  /** Stops the transfer in this page. The session stays open in Cloud until abort() or its expiry. */
  signal?: AbortSignal;
  /** Called after each part: progress moves in steps of one part. */
  onProgress?: (progress: UploadProgress) => void;
  /** Parts sent at the same time: 1 to 16, default 4. */
  concurrency?: number;
  /**
   * Compare a CRC-64/NVME computed here with the one storage computed, and reject the upload
   * when they differ. It reads the file a second time. Default: on for files up to 1 GiB.
   */
  checksum?: boolean;
  fetch?: typeof globalThis.fetch;
}

export interface UploadOptions extends Omit<TransferOptions, 'fetch'> {
  /** The upload's id. Use the same id again to continue an upload that stopped. Default: a new UUID. */
  id?: string;
  /** Required when the source is not a File. */
  name?: string;
  /** The media type to declare. Default: the File's type, or application/octet-stream. */
  type?: string;
  /** Anything the backend needs to decide, such as the record the file belongs to. */
  context?: unknown;
  /** Called with the session as soon as the backend returned it. */
  onSession?: (session: UploadSession) => void;
}

export type UploadErrorCode =
  /** The backend refused, or the session token is not valid. */
  | 'UPLOAD_AUTHORIZATION'
  /** The signal stopped the transfer. */
  | 'UPLOAD_CANCELLED'
  /** The session is aborted, expired or unknown. */
  | 'UPLOAD_SESSION'
  /** The stored bytes differ from the file: its checksum did not match. Nothing was published. */
  | 'UPLOAD_REJECTED'
  /** The network or the service failed after retries. */
  | 'UPLOAD_TRANSPORT'
  /** The service refused for a rate limit after retries. */
  | 'UPLOAD_LIMIT'
  /** The upload changed during completion; the transfer retries this itself. */
  | 'UPLOAD_CONFLICT'
  /** The file is not the one the session was opened for, or it is empty. */
  | 'UPLOAD_SOURCE'
  /** A reply that the client does not understand. */
  | 'UPLOAD_PROTOCOL';

export class UploadError extends Error {
  readonly code: UploadErrorCode;
  /** True when upload() with the same `id` can continue. False: start a new upload. */
  readonly recoverable: boolean;
  readonly status?: number;
  /** The session in use, when there was one. Not enumerable, so it is not logged by accident. */
  readonly session?: UploadSession;
  readonly cause?: unknown;
}

export interface Uploader {
  /** Uploads a file and resolves with it once Cloud has published it. */
  upload(source: Blob | ArrayBuffer | ArrayBufferView, options?: UploadOptions): Promise<ManagedFile>;
  status(session: UploadSession, options?: { signal?: AbortSignal }): Promise<UploadStatus>;
  /** Ends the upload in Cloud and frees its reserved space. */
  abort(session: UploadSession, options?: { signal?: AbortSignal }): Promise<UploadStatus>;
}

/**
 * `authorize` calls the page's own backend, which checks its user and returns the result of
 * `db.files.createUploadSession()`.
 */
export function createUploader(options: {
  authorize: (intent: UploadIntent) => Promise<UploadSession>;
  fetch?: typeof globalThis.fetch;
  concurrency?: number;
}): Uploader;

/** Sends a file through a session you already hold. Parts storage has are not sent again. */
export function uploadToSession(session: UploadSession, source: Blob | ArrayBuffer | ArrayBufferView, options?: TransferOptions): Promise<ManagedFile>;

/** A running CRC-64/NVME, the checksum storage reports for a file. */
export function crc64nvme(): { update(bytes: Uint8Array): ReturnType<typeof crc64nvme>; base64(): string };
