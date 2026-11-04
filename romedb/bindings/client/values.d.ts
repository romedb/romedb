export class Float64 { constructor(value: number); readonly value: number; }
export class Record { constructor(table: string, key: string | bigint); readonly table: string; readonly key: string | bigint; /** `table:key`. */ toString(): string; /** The same table and key, including the key's type. */ equals(other: unknown): boolean; }
export class Vector {
  constructor(bytes: Uint8Array);
  /** A copy of the encoded bytes. */
  readonly bytes: Uint8Array;
  static float32(values: readonly number[] | Float32Array | Float64Array): Vector;
  static float64(values: readonly number[] | Float32Array | Float64Array): Vector;
  static sparse32Entries(dimensions: number, entries: readonly (readonly [number, number])[]): Vector;
}
/** Binary values decode as plain Uint8Array (not Buffer) from 0.2.12. */
export type Value = null | boolean | bigint | number | string | Float64 | Record | Vector | Uint8Array | Value[] | { [key: string]: Value };
export type PortableValue = { type: 'Null' } | { type: 'Boolean'; value: boolean } | { type: 'Integer' | 'Number' | 'String'; value: string } | { type: 'Binary' | 'Vector'; value: number[] | string } | { type: 'Record'; value: { table: string; key: { type: 'Integer' | 'String'; value: string } } } | { type: 'Array'; value: PortableValue[] } | { type: 'Object'; value: { [key: string]: PortableValue } };
export function encodeValue(value: Value): PortableValue;
export function decodeValue(value: unknown): Value;
