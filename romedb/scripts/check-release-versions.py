#!/usr/bin/env python3
"""Reject mixed RomeDB identities before release packaging; leave Turso untouched."""
import json
from pathlib import Path
import re
import tomllib
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
version = json.loads((ROOT / "romedb/release.json").read_text())["version"]
if not re.fullmatch(r"(0|[1-9][0-9]*)\.[0-9]+\.[0-9]+", version):
    raise ValueError("Release version must be a stable semantic version")
crates = ["frontend", "parser", "cli", "service", "tests", "bindings/node", "bindings/python", "bindings/protocol", "bindings/c"]
lock = tomllib.loads((ROOT / "Cargo.lock").read_text())["package"]
for crate in crates:
    manifest = tomllib.loads((ROOT / "romedb" / crate / "Cargo.toml").read_text())["package"]
    assert manifest["version"] == version, crate
    entries = [p for p in lock if p["name"] == manifest["name"] and "source" not in p]
    assert len(entries) == 1 and entries[0]["version"] == version, crate + " lock"
assert json.loads((ROOT / "romedb/bindings/php/composer.json").read_text())["version"] == version
node = json.loads((ROOT / "romedb/bindings/node/package.json").read_text())
assert node["version"] == version
npm = json.loads((ROOT / "romedb/bindings/node/package-lock.json").read_text())
assert npm["version"] == npm["packages"][""]["version"] == version
python = tomllib.loads((ROOT / "romedb/bindings/python/pyproject.toml").read_text())
assert python["project"]["version"] == version
assert f'__version__ = "{version}"' in (ROOT / "romedb/bindings/python/python/romedb/__init__.py").read_text()
assert ET.parse(ROOT / "romedb/bindings/csharp/RomeDB/RomeDB.csproj").findtext("PropertyGroup/Version") == version
module = (ROOT / "romedb/bindings/go/go.mod").read_text().splitlines()[0]
major = int(version.split('.')[0])
assert module == "module github.com/romedb/romedb/romedb/bindings/go" + (f"/v{major}" if major >= 2 else "")
assert '"romedb/bindings/browser"' not in (ROOT / "Cargo.toml").read_text()
print(f"All nine RomeDB crates and native client package identities match {version}")
