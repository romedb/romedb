#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo build --locked -p romedb-c
case "$(uname -s)" in
  Linux) library=libromedb_c.so ;;
  Darwin) library=libromedb_c.dylib ;;
  *) echo 'C ABI checks currently support Linux and macOS hosts' >&2; exit 1 ;;
esac
python3 romedb/scripts/check-c-abi.py "${CARGO_TARGET_DIR:-target}/debug/$library"
