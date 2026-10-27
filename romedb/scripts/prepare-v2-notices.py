#!/usr/bin/env python3
"""Regenerate native release inventories and checksum-verified source notices."""
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
for short, package, output in [
    ("node", "romedb-node", "bindings/node/THIRD_PARTY_CRATE_NOTICES.md"),
    ("python", "romedb-python", "bindings/python/THIRD_PARTY_CRATE_NOTICES.md"),
    ("c", "romedb-c", "bindings/c/THIRD_PARTY_CRATE_NOTICES.md"),
    ("cli", "romedb-cli", "docs/cli-crate-notices-linux-x64.md"),
]:
    inventory = f"romedb/docs/{short}-dependencies-linux-x64.json"
    audit = f"romedb/docs/{short}-crate-notices-linux-x64.json"
    env = dict(os.environ, ROMEDB_INVENTORY_PACKAGE=package)
    for command in [
        ["node", "romedb/scripts/inventory-node-dependencies.cjs", "x86_64-unknown-linux-gnu", inventory],
        ["python3", "romedb/scripts/audit-crate-notices.py", inventory, audit],
        ["python3", "romedb/scripts/bundle-crate-notices.py", inventory, audit, "romedb/" + output,
         "--supplements", "romedb/docs/notice-source-supplements.json"],
    ]:
        subprocess.run(command, cwd=ROOT, env=env, check=True)
