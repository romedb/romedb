# RomeDB for Go

Native embedded RomeDB/RomeQL through cgo. This is a local development module;
see [qualification and limits](../../docs/native-language-clients.md).

Build the native library from the repository root:

```sh
cargo build --locked -p romedb-c
```

Use a local `replace` for module
`github.com/romedb/romedb/romedb/bindings/go` pointing at this directory.
Set `CGO_LDFLAGS=-L/absolute/path/to/target/debug` for linking and
`LD_LIBRARY_PATH=/absolute/path/to/target/debug` on Linux for execution.

```go
db, err := romedb.Open("app.db")
if err != nil { return err }
defer db.Close()
result, err := db.Execute("SELECT $n", map[string]romedb.Value{
    "$n": romedb.Integer(9223372036854775807),
}, -1)
if err != nil { return err }
n, err := result.Rows[0][0].Int64()
```

`Call` exposes the shared protocol. `*romedb.Error` includes `Code`, `Message`
and transaction state. Timeouts are milliseconds, with `-1` for none.
Call `Close` explicitly, and synchronize an entire multi-call transaction.
Run `go test -race -v ./...` with the linker/loader paths configured.
