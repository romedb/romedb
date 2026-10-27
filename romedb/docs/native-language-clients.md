# PHP, Swift, C# and Go native clients

The requested V2 client set is Rust, Node.js/TypeScript, Python, PHP, Swift, C# and
Go. Browser/WASM remains removed and cloud remains outside this workstream.

The four new clients embed the RomeDB frontend through `romedb-c`, a shared
native library. Their operations use the existing `romedb-protocol` request and
transfer-v1 value formats. The implementation changes no upstream core source
and adds no third-party Rust dependency. Each process opens a local database
file; it does not require a database server or Node/Python subprocess.

## Packages

| Language | Source package | Native interface | Local qualification |
|---|---|---|---|
| PHP | `bindings/php`, Composer `romedb/embedded` | PHP FFI | PHP 8.3.6, Linux x86_64 |
| Swift | `bindings/swift`, SwiftPM `RomeDB` | Clang C module | Swift 6.4, Linux x86_64 |
| C# | `bindings/csharp/RomeDB`, NuGet `RomeDB.Embedded` | P/Invoke | .NET SDK 8.0.425, Linux x86_64 |
| Go | `bindings/go` | cgo | Go 1.27.1, Linux x86_64 |

Final 2.0.0 packages are qualified and distributed in the
[release bundle](release-2.0.0.md). V2 binary
distribution is Linux x64 only. Registry publication, macOS/Windows/ARM and Apple
mobile support are outside this release.
The manifest language minimums are not a claim that every runtime between that
minimum and the tested runtime has been qualified. PHP requires a 64-bit runtime
with FFI enabled. Go requires cgo and a C compiler. Swift and C# need the native
library available to the system loader.

## Build and test

From the engine checkout, with Rust 1.88.0 and the language toolchains installed:

```sh
cargo build --locked -p romedb-c
bash romedb/scripts/check-native-languages.sh
```

The current verification script targets Linux. It formats/lints the C ABI,
builds a development shared library, checks distributed headers match the ABI,
runs ownership/concurrency tests, then all four language contract suites. Go
also runs its race detector. It requires all four toolchains and fails on an
unavailable toolchain rather than silently skipping a client. It is separate
from the routine Node/Rust check to avoid installing four toolchains on every
unrelated change. The C ABI itself is included in routine scoped formatting and
Clippy checks.

`romedb/bindings/c/include/romedb.h` defines ABI version 1. All returned strings
are library-owned until freed exactly once with `fdb_free`; wrappers do this in
their cleanup paths. Inputs are borrowed UTF-8 C strings. Callers must provide
valid pointers and must not unload the library while handles remain live.
Handles are opaque process-local integers, never reused; use each wrapper's
close/dispose mechanism. They cannot cross a process fork.

Calls on a connection serialize in native code. A transaction spanning multiple
calls requires exclusive caller ownership of that connection for the whole
transaction. Closing waits for an active operation, rolls back uncommitted work
and rejects later calls. The handle stays interruptible while close waits.
Interruption is cooperative and targets active statements. Timeouts are
milliseconds from native call entry, including queue wait, checked cooperatively;
they do not forcibly terminate a lock wait. Use `-1` for no deadline.

## API and values

Each client has `open`/constructor, `execute`, `call`, `interrupt` and
`close`/dispose. `execute` accepts SQL/RomeQL with explicit named parameters and
returns columns, positional rows, affected count and transaction before/after.
Duplicate column names retain both positions. `call` also exposes profiling,
bounded reads/writes, batches, migrations, document transfer, integrity, vector
construction and transaction state through the shared request schema.

Parameter helpers construct tagged values. Results retain the tags so no client
silently narrows int64, confuses binary/vector data or loses typed record IDs.
Integers use decimal strings, floating-point numbers use binary64 hex bits,
binary/vector bytes use integer arrays, and records contain a table plus tagged
key. Null, boolean, string, object and array are distinct. PHP preserves object
payloads as `stdClass`, including empty objects. Go provides `Value.Int64()`;
other returned tagged values can be inspected directly. See
[transfer format](transfer.md) for the complete encoding.

Structured errors retain the engine code, message, optional migration diagnostic
and transaction before/after when execution reached the connection. A batch's
statement errors are in its per-statement reports; callers must inspect them.
These direct APIs do not currently implement PDO, ADO.NET or Go `database/sql`
driver interfaces, async collection helpers or automatic transaction callbacks.

## Evidence

`/tmp/romedb-native-languages.log` records the local integrated suite. The C#
Unicode rejection follow-up also passes all 42 steps in
`/tmp/romedb-csharp-final.log`. The tested development library is
`target/debug/libromedb_c.so` (305,371,424 bytes), SHA-256
`d138c118803f4bc6750dcae5b8f222859daa079f0793fd57bb80f1c57534115d`.
It is a local Linux x86_64 artifact built with Rust 1.88.0, not a final release
binary. The shared
42-step fixture tests int64 extrema, negative zero/subnormal doubles, Unicode and
embedded NUL text, binary, nested objects/arrays, typed records, duplicate result
columns, parameter rejection, unique indexes, savepoints, rollback, zero timeout,
FTS/ANN/spatial/H3, stored JavaScript error recovery, committed persistence,
close-time rollback, FTS drop integrity, collection integrity and vector creation.
Each wrapper also tests its value constructors and use after close.

`python3 romedb/scripts/check-native-language-packages.py` passes all four
isolated consumers: copied PHP and Go source packages, a separate SwiftPM
application and a .NET application restored from a locally packed NuGet archive
with an isolated package cache. Log: `/tmp/romedb-native-language-packages.log`.
The native library is supplied separately in each test; these checks do not
claim bundled platform distribution.

Three ctypes boundary tests exercise malformed requests, invalid UTF-8/null input,
double close, stale handles, twelve concurrent connection lifecycles and
interruption while close waits. These tests operate on the actual shared library.
No benchmark or untested platform claim follows from them.

Native final artifacts must still include full dependency attribution and pass
the V1 upgrade/restore rehearsal on each advertised platform. The shared library
inherits the engine's existing native notice gaps; it does not introduce a new
third-party dependency or a new maintained core exception. The C dependency
inventory contains 258 declarations and 182 collected notice texts, with four
existing external gaps. See `c-dependencies-linux-x64.json`,
`c-crate-notices-linux-x64.json` and
`bindings/c/THIRD_PARTY_CRATE_NOTICES.md` (relative to `romedb/`).

Cargo metadata compared with the preceding native-only tree adds only
`romedb-c 2.0.0-dev.1`; no package identity was removed or upgraded. The lock
SHA-256 is `1f64d71cc6cb57e691be4a7bc9b5a39ef4d00721515831813d1442aca2941aaa`.

Implementation references: [PHP FFI](https://www.php.net/manual/en/ffi.cdef.php),
[Go cgo](https://pkg.go.dev/cmd/cgo),
[SwiftPM system libraries](https://docs.swift.org/package-manager/PackageDescription/PackageDescription.html),
[.NET native interop](https://learn.microsoft.com/en-us/dotnet/standard/native-interop/).
