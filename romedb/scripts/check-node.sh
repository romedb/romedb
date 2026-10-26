#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
build_profile=debug
cargo_build_args=()
strip_debug=false
case "${1:-}" in
  '') ;;
  --release) build_profile=release; cargo_build_args+=(--release) ;;
  --release-stripped) build_profile=release; cargo_build_args+=(--release); strip_debug=true ;;
  *) echo 'Usage: check-node.sh [--release|--release-stripped]' >&2; exit 2 ;;
esac
if (( $# > 1 )); then
  echo 'Usage: check-node.sh [--release|--release-stripped]' >&2
  exit 2
fi
if $strip_debug; then
  if [[ "$(uname -s)" != Linux ]]; then
    echo '--release-stripped currently supports Linux only' >&2
    exit 2
  fi
  command -v strip >/dev/null || { echo 'strip is required for --release-stripped' >&2; exit 2; }
fi
cargo build --locked -p romedb-node "${cargo_build_args[@]}"
case "$(uname -s)" in
  Linux) native_library=libromedb_node.so ;;
  Darwin) native_library=libromedb_node.dylib ;;
  *) echo 'Native build script currently supports Linux and macOS only' >&2; exit 1 ;;
esac
cp "${CARGO_TARGET_DIR:-target}/$build_profile/$native_library" romedb/bindings/node/romedb.node
if $strip_debug; then
  # Strip only the package copy; retain the full Cargo artifact for diagnostics.
  strip --strip-debug romedb/bindings/node/romedb.node
fi
node --test --test-concurrency=2 romedb/bindings/node/test.cjs romedb/bindings/node/ownership.test.cjs romedb/bindings/node/client.test.cjs romedb/bindings/node/client-native.test.cjs romedb/bindings/node/embedded-client.test.cjs romedb/examples/node-task-tracker/test.cjs
if [[ "${ROMEDB_SKIP_TYPECHECK:-}" != 1 ]]; then
  pnpm --dir romedb/bindings/node run typecheck
fi
