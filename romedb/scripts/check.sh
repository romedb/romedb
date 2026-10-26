#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
python3 romedb/scripts/test_shipping_policy.py
python3 romedb/scripts/test_upgrade_matrix.py
cargo fmt -p romeql-parser -p romedb -p romedb-cli -p romedb-service -p romedb-tests -p romedb-node -p romedb-python -p romedb-protocol -p romedb-c -- --check
cargo clippy --locked -p romeql-parser -p romedb -p romedb-cli -p romedb-service -p romedb-tests -p romedb-node -p romedb-python -p romedb-protocol -p romedb-c --all-targets --no-deps -- -D warnings
cargo test --locked -p romeql-parser -p romedb -p romedb-cli -p romedb-service -p romedb-tests
romedb/scripts/check-node.sh
romedb/scripts/check-c.sh
