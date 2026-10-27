# RomeDB for Swift

Native SwiftPM library using RomeDB's C ABI. This is a local development package;
see [qualification and limits](../../docs/native-language-clients.md).

Build the native library with `cargo build --locked -p romedb-c`. Add this
directory as a local SwiftPM package dependency, then add product `RomeDB` to
your application target. The included C header is self-contained. Supply the
native library's directory using linker `-L` and runtime `rpath` flags.

```swift
import RomeDB

let db = try Database(path: "app.db")
defer { try? db.close() }
let result = try db.execute("SELECT $n", parameters: ["$n": .integer(Int64.max)])
let rows = result["rows"] as! [[[String: Any]]]
print(rows[0][0]["value"]!) // decimal string, preserving int64
```

`call` exposes the complete shared protocol. `RomeDBError` provides a diagnostic,
code and transaction report. Operations are synchronous; use a caller-owned
serial executor for grouped transactions. `deinit` closes the connection.
The root `check-native-languages.sh` supplies the test fixture and Linux linker
paths. Apple platform binaries and app integration are not yet qualified.
