import { Float64, Record, Vector, Value, PortableValue, encodeValue, decodeValue } from '@romedb/node/values';
import { ClientError, RemoteDatabase, connect } from '@romedb/node/client';

const portable: PortableValue = encodeValue({ id: new Record('posts', 1n), score: new Float64(0.5), embedding: Vector.float32([1, 2]), raw: new Uint8Array([1]) });
const value: Value = decodeValue(portable);
const bytes: Uint8Array = Vector.float64([1]).bytes;
void value; void bytes;

async function webClient(): Promise<void> {
  const db: RemoteDatabase = await connect({ url: 'http://127.0.0.1:4321', token: 'test' });
  const cloud: boolean = db.cloud;
  const known: boolean = db.sequenceKnown;
  void cloud; void known;
}
void webClient;
function backoff(error: unknown): number | undefined {
  if (error instanceof ClientError) {
    const cause: unknown = error.cause;
    void cause;
    return error.retryAfterMs;
  }
  return undefined;
}
void backoff;
