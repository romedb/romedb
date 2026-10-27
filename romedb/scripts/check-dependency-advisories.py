#!/usr/bin/env python3
"""Review the Linux shipping closure against a pinned, local RustSec database."""

import argparse
import datetime
import hashlib
import json
from pathlib import Path
import re
import subprocess


ROOT = Path(__file__).resolve().parents[2]
PACKAGES = ["romedb-cli", "romedb-node", "romedb-python", "romedb-c"]
TARGET = "x86_64-unknown-linux-gnu"


def command(args, cwd=ROOT):
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", required=True, type=Path)
    parser.add_argument("--cargo-audit", default="cargo-audit")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument(
        "--allow-informational", action="append", default=[], metavar="RUSTSEC-ID",
        help="An explicitly reviewed, non-applicable informational warning; document why separately",
    )
    args = parser.parse_args()
    database = args.database.resolve()
    if command(["git", "status", "--porcelain"], database):
        parser.error("advisory database must have no local modifications")
    tree = command([
        "cargo", "tree", "--locked", "--target", TARGET, "-e", "normal,build",
        "--prefix", "none", "--format", "{p}",
        *[arg for package in PACKAGES for arg in ("-p", package)],
    ])
    packages = set()
    for line in tree.splitlines():
        if not line.strip():
            continue
        match = re.match(r"(\S+) v(\S+)(?:\s|$)", line)
        if not match:
            raise RuntimeError(f"unexpected cargo tree line: {line!r}")
        packages.add(match.groups())
    result = subprocess.run([
        args.cargo_audit, "audit", "--file", "Cargo.lock", "--db", str(database),
        "--no-fetch", "--no-yanked", "--target-arch", "x86_64", "--target-os", "linux",
        "--json",
    ], cwd=ROOT, check=False, text=True, capture_output=True)
    if result.returncode not in (0, 1):
        raise RuntimeError(result.stderr or f"cargo-audit failed: {result.returncode}")
    audit = json.loads(result.stdout)
    findings, excluded = [], []
    groups = {"vulnerability": audit["vulnerabilities"]["list"], **audit["warnings"]}
    for kind, group in groups.items():
        for item in group:
            package, advisory = item["package"], item["advisory"]
            entry = {
                "kind": kind, "package": package["name"], "version": package["version"],
                "id": advisory["id"], "title": advisory["title"],
                "url": f"https://rustsec.org/advisories/{advisory['id']}.html",
            }
            if (package["name"], package["version"]) in packages:
                entry["accepted_informational"] = (
                    kind != "vulnerability" and advisory["id"] in args.allow_informational
                )
                findings.append(entry)
            else:
                excluded.append(entry)
    unresolved = [item for item in findings if not item["accepted_informational"]]
    receipt = {
        "reviewed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "source_commit": command(["git", "rev-parse", "HEAD"]),
        "cargo_lock_sha256": hashlib.sha256((ROOT / "Cargo.lock").read_bytes()).hexdigest(),
        "advisory_database": {
            "repository": command(["git", "remote", "get-url", "origin"], database),
            "commit": command(["git", "rev-parse", "HEAD"], database),
            "committed_at": command(["git", "show", "-s", "--format=%cI", "HEAD"], database),
            "advisory_count": audit["database"]["advisory-count"],
        },
        "scanner": command([args.cargo_audit, "--version"]),
        "target": TARGET, "roots": PACKAGES, "edge_kinds": ["normal", "build"],
        "packages": [{"name": name, "version": version} for name, version in sorted(packages)],
        "findings": findings, "excluded_workspace_findings": excluded,
        "unresolved_count": len(unresolved),
        "limits": [
            "Version/target reachability, not compiled-function reachability or exploitability analysis",
            "Dev-only and non-Linux dependencies excluded; yanked-package check disabled",
            "C/C++ libraries, language runtimes, system packages and source code require separate review",
        ],
    }
    args.output.write_text(json.dumps(receipt, indent=2) + "\n")
    print(f"{len(packages)} shipping packages; {len(findings)} findings; {len(unresolved)} unresolved")
    raise SystemExit(bool(unresolved))


if __name__ == "__main__":
    main()
