# RomeDB for C# / .NET

Native embedded RomeDB/RomeQL using P/Invoke, targeting .NET 8. This is a local
development package; see [qualification and limits](../../docs/native-language-clients.md).

Build the native library with `cargo build --locked -p romedb-c`, reference
`RomeDB/RomeDB.csproj`, and make `libromedb_c.so` available to the loader on Linux
(for example through `LD_LIBRARY_PATH`). Native binaries are not yet bundled in
NuGet platform assets. Local packaging uses `dotnet pack RomeDB/RomeDB.csproj
-c Debug`.

```csharp
using RomeDB;
using System.Text.Json.Nodes;

using var db = new Database("app.db");
var result = db.Execute("SELECT $n", new JsonObject { ["$n"] = Value.Integer(long.MaxValue) });
Console.WriteLine(result["rows"]![0]![0]!["value"]); // lossless decimal string
```

`Call` exposes the shared protocol, and `RomeDBException` exposes `Code`,
`Diagnostic` and `Transaction`. Operations are synchronous. Own the connection
exclusively for a transaction spanning several calls. Dispose deterministically;
the finalizer is a fallback. Run the integration executable from the repository:

```sh
dotnet run --project romedb/bindings/csharp/Tests -- romedb/bindings/fixtures/native-client.json
```
