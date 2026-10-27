# RomeDB for PHP

Native embedded RomeDB/RomeQL for 64-bit PHP with the FFI extension. This is a
local development package; see [qualification and limits](../../docs/native-language-clients.md).

Build `libromedb_c.so` with `cargo build --locked -p romedb-c` in the repository
root. Use Composer's classmap autoload or require `src/RomeDB.php` directly.
Pass an absolute library path:

```php
require 'src/RomeDB.php';
use RomeDB\Database;
use RomeDB\Value;

$db = new Database('app.db', '/absolute/path/libromedb_c.so');
try {
    $result = $db->execute('SELECT $name', ['$name' => Value::string('hello')]);
    echo $result['rows'][0][0]['value'];
} finally {
    $db->close();
}
```

`call()` exposes the shared protocol and `interrupt()` requests cooperative
cancellation. `RomeDBException` provides `diagnostic` and `transaction`.
Values retain lossless tags in results. Run the local fixture with:

```sh
ROMEDB_LIBRARY=/absolute/path/libromedb_c.so php -d ffi.enable=1 tests/smoke.php
```

The application's PHP deployment must permit FFI; CLI enablement does not
configure PHP-FPM. Never carry an open handle across `fork()`.
