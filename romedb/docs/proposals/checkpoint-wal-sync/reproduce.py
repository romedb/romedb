#!/usr/bin/env python3
"""Run only the proposed checkpoint regressions against a supplied checkout."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tomllib

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("checkout", type=Path)
parser.add_argument("evidence", type=Path)
parser.add_argument("--target-dir", type=Path)
args = parser.parse_args()
checkout = args.checkout.resolve()
evidence = args.evidence.resolve()
evidence.mkdir(parents=True, exist_ok=False)
proposal = Path(__file__).resolve().parent
target = args.target_dir.resolve() if args.target_dir else checkout / "target"
versions = {package["name"]: package["version"] for package in tomllib.loads((checkout / "Cargo.lock").read_text())["package"]}
manifest = evidence / "Cargo.toml"
manifest.write_text(f'''[package]
name="romedb-checkpoint-wal-sync-repro"
version="0.0.0"
edition="2021"
[workspace]
resolver="2"
[dependencies]
romedb={{path={json.dumps(str(checkout / "romedb/frontend"))}}}
turso_core={{path={json.dumps(str(checkout / "core"))},features=["conn_raw_api","fts"]}}
anyhow="={versions['anyhow']}"
tempfile="={versions['tempfile']}"
[[bin]]
name="checkpoint-wal-sync-repro"
path={json.dumps(str(proposal / "repro.rs"))}
[[test]]
name="crash_atomicity"
path={json.dumps(str(proposal / "crash_atomicity.rs"))}
[[test]]
name="boundaries"
path={json.dumps(str(proposal / "boundaries.rs"))}
''')
shutil.copyfile(checkout / "Cargo.lock", evidence / "Cargo.lock")
results = {}
for name, action, extra in [("trace", "run", []),
                            ("crash", "test", ["--test", "crash_atomicity", "--", "--nocapture"]),
                            ("boundaries", "test", ["--test", "boundaries", "--", "--nocapture"])]:
    with (evidence / f"{name}.log").open("w") as log:
        result = subprocess.run(["cargo", action, "--offline", "--manifest-path", str(manifest),
                                 "--target-dir", str(target), *extra], cwd=checkout,
                                stdout=log, stderr=subprocess.STDOUT)
    results[name] = result.returncode
report = {"checkout": str(checkout), "results": results,
          "head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=checkout, text=True).strip(),
          "sourceSha256": {name: hashlib.sha256((checkout / name).read_bytes()).hexdigest()
                           for name in ["core/storage/wal.rs", "core/storage/pager.rs", "core/vdbe/mod.rs",
                                        "core/vdbe/vacuum.rs", "core/mvcc/database/mod.rs",
                                        "core/mvcc/database/checkpoint_state_machine.rs", "Cargo.lock"]},
          "regressionSha256": {name: hashlib.sha256((proposal / name).read_bytes()).hexdigest()
                               for name in ["repro.rs", "crash_atomicity.rs", "boundaries.rs"]},
          "patchSha256": hashlib.sha256((proposal / "backport.patch").read_bytes()).hexdigest()}
(evidence / "evidence.json").write_text(json.dumps(report, indent=2) + "\n")
print(json.dumps(report))
raise SystemExit(int(any(results.values())))
