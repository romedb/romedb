// Capture conversion primitives before user code can change any globals.
(() => {
  'use strict';
  const parse = JSON.parse, stringify = JSON.stringify;
  const create = Object.create, keys = Reflect.ownKeys;
  const descriptor = Object.getOwnPropertyDescriptor, proto = Object.getPrototypeOf;
  const setProto = Object.setPrototypeOf, objectProto = Object.prototype;
  const isArray = Array.isArray, finite = Number.isFinite, bigint = BigInt, same = Object.is;
  const hasOwn = Function.call.bind(Object.prototype.hasOwnProperty);
  const codeAt = Function.call.bind(String.prototype.charCodeAt);
  const validString = (v) => {
    for (let i = 0; i < v.length; i++) {
      const c = codeAt(v, i);
      if (c >= 0xd800 && c <= 0xdbff) {
        const n = codeAt(v, ++i);
        if (!(n >= 0xdc00 && n <= 0xdfff)) fail();
      } else if (c >= 0xdc00 && c <= 0xdfff) fail();
    }
  };
  const ResultError = TypeError;
  const fail = () => { throw new ResultError('unsupported JavaScript result'); };
  const tag = (type, value) => {
    const result = create(null); result.type = type;
    if (type !== 'Null') result.value = value;
    return result;
  };
  const decode = (v) => {
    switch (v.type) {
      case 'Null': return null;
      case 'Integer': return bigint(v.value);
      case 'Object': {
        const out = create(null);
        for (const key of keys(v.value)) out[key] = decode(v.value[key]);
        return out;
      }
      case 'Array': {
        const out = [];
        for (let i = 0; i < v.value.length; i++) out[i] = decode(v.value[i]);
        return out;
      }
      default: return v.value;
    }
  };
  const encode = (v, depth, ancestors) => {
    if (depth > 64) fail();
    if (v === null) return tag('Null');
    switch (typeof v) {
      case 'boolean': return tag('Boolean', v);
      case 'string': {
        validString(v);
        return tag('String', v);
      }
      case 'number': if (!finite(v)) fail(); return tag('Number', same(v, -0) ? '-0' : v);
      case 'bigint':
        if (v < -9223372036854775808n || v > 9223372036854775807n) fail();
        return tag('Integer', `${v}`);
      case 'object': break;
      default: fail();
    }
    for (let i = 0; i < ancestors.length; i++) if (ancestors[i] === v) fail();
    ancestors[ancestors.length] = v;
    let result;
    if (isArray(v)) {
      const n = v.length;
      if (n > 65536 || keys(v).length !== n + 1) fail();
      const out = []; setProto(out, null);
      for (let i = 0; i < n; i++) {
        const d = descriptor(v, `${i}`);
        if (!d || !hasOwn(d, 'value')) fail();
        out[i] = encode(d.value, depth + 1, ancestors);
      }
      result = tag('Array', out);
    } else {
      if (proto(v) !== null && proto(v) !== objectProto) fail();
      const out = create(null);
      const names = keys(v);
      for (let i = 0; i < names.length; i++) {
        const key = names[i];
        if (typeof key !== 'string') fail();
        validString(key);
        const d = descriptor(v, key);
        if (!d || !d.enumerable || !hasOwn(d, 'value')) fail();
        out[key] = encode(d.value, depth + 1, ancestors);
      }
      result = tag('Object', out);
    }
    ancestors.length--;
    return result;
  };
  // No clock, random source, weak-reference GC observations, or host bindings.
  delete globalThis.Date;
  delete globalThis.performance;
  delete Math.random;
  delete globalThis.WeakRef;
  delete globalThis.FinalizationRegistry;
  delete globalThis.SharedArrayBuffer;
  delete globalThis.Atomics;
  return (fn, input) => {
    const args = parse(input);
    for (let i = 0; i < args.length; i++) args[i] = decode(args[i]);
    const ancestors = []; setProto(ancestors, null);
    return stringify(encode(fn(...args), 0, ancestors));
  };
})()
