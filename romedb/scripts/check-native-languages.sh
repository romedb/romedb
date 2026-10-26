#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
root="$PWD"
cargo fmt -p romedb-c -- --check
cargo clippy --locked -p romedb-c --all-targets --no-deps -- -D warnings
cargo build --locked -p romedb-c
export LD_LIBRARY_PATH="${CARGO_TARGET_DIR:-$root/target}/debug${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export ROMEDB_LIBRARY="${CARGO_TARGET_DIR:-$root/target}/debug/libromedb_c.so"
export ROMEDB_FIXTURE="$root/romedb/bindings/fixtures/native-client.json"
export CGO_ENABLED=1
export CGO_LDFLAGS="-L${CARGO_TARGET_DIR:-$root/target}/debug"
cmp romedb/bindings/c/include/romedb.h romedb/bindings/go/include/romedb.h
cmp romedb/bindings/c/include/romedb.h romedb/bindings/swift/Sources/CRomeDB/romedb.h
python3 romedb/scripts/check-c-abi.py "$ROMEDB_LIBRARY"
php -d ffi.enable=1 romedb/bindings/php/tests/smoke.php
(cd romedb/bindings/go && go test -race -v ./...)
DOTNET_CLI_TELEMETRY_OPTOUT=1 dotnet run --project romedb/bindings/csharp/Tests -- "$ROMEDB_FIXTURE"
(cd romedb/bindings/swift && swift test -j 2 -Xlinker "-L${CARGO_TARGET_DIR:-$root/target}/debug" -Xlinker -rpath -Xlinker "${CARGO_TARGET_DIR:-$root/target}/debug")
