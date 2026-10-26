#!/usr/bin/env python3
"""Dry-run every previous-version upgrade/restore check against a candidate Node
package before tagging, using the same table as bundle qualification.

The candidate can be the development package (romedb/bindings/node with a built
romedb.node) or an extracted release package. A failure here means a script or
the upgrade table needs a change, which is cheaper to fix before the build commit
is tagged than after the bundle is built.
"""
import argparse
import json
from pathlib import Path
import subprocess

import upgrade_matrix

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path, help="New directory for logs and fixtures")
    parser.add_argument("--package", type=Path, required=True, help="Candidate Node package directory")
    parser.add_argument("--node", action="append", required=True, help="Node executable; repeat for each runtime")
    parser.add_argument("--previous-package", action="append", default=[], metavar="VERSION=PATH")
    args = parser.parse_args()
    package = args.package.resolve()
    candidate = json.loads((package / "package.json").read_text())["version"]
    release = json.loads((ROOT / "romedb/release.json").read_text())["version"]
    assert candidate == release, f"package is {candidate}, release.json is {release}"
    packages = upgrade_matrix.previous_packages(args.previous_package, candidate)
    evidence = args.evidence.resolve()
    evidence.mkdir(parents=True)

    def run(name, command):
        with (evidence / f"{name}.log").open("w") as log:
            subprocess.run(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT, check=True)
        print(f"{name} passed", flush=True)

    upgrade_matrix.run_upgrades(run, ROOT, candidate, package, packages, args.node, evidence)
    print(f"{candidate}: {len(upgrade_matrix.applicable(candidate))} upgrade checks pass on {len(args.node)} runtime(s)")


if __name__ == "__main__":
    main()
