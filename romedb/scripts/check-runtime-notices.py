#!/usr/bin/env python3
"""Check preserved Rust standard-library notices against the pinned toolchain."""
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
record = json.loads((ROOT / "romedb/docs/rust-runtime-notices.json").read_text())
version = subprocess.check_output(["rustc", "--version"], text=True).split()[1]
assert version == record["rustVersion"], "Requalify runtime notices for the new compiler"
sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
paths = [sysroot / record["sourcePath"]]
paths += [ROOT / "romedb/bindings" / client / "RUST-LIBRARY-NOTICES.html" for client in ["node", "python", "c"]]
for path in paths:
    assert hashlib.sha256(path.read_bytes()).hexdigest() == record["fileSha256"], str(path)
print("Pinned Rust runtime notice and all three packaged copies match")
