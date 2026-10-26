#!/usr/bin/env python3
"""Install a built wheel offline in a fresh environment and run client contracts."""

import argparse
import os
import shutil
import subprocess
import tempfile
import zipfile
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("wheel", type=Path)
    parser.add_argument("--python", required=True, help="Installed CPython executable")
    parser.add_argument("--uv", default="uv", help="uv executable used for offline installation")
    args = parser.parse_args()
    wheel = args.wheel.resolve(strict=True)
    python = shutil.which(args.python)
    uv = shutil.which(args.uv)
    if not python or not uv:
        parser.error("Python and uv must already be installed")
    with zipfile.ZipFile(wheel) as archive:
        names = archive.namelist()
        required = {"romedb/__init__.py", "romedb/_native.pyi", "romedb/py.typed"}
        if not required.issubset(names):
            raise ValueError("Wheel lacks Python API or typing files")
        for notice in ("LICENSE.md", "THIRD_PARTY_NOTICES.md", "THIRD_PARTY_CRATE_NOTICES.md", "RUST-LIBRARY-NOTICES.html"):
            if not any(name.endswith(".dist-info/licenses/" + notice) for name in names):
                raise ValueError(f"Wheel lacks {notice}")
    environment = dict(os.environ, UV_PYTHON_DOWNLOADS="never", UV_OFFLINE="1")
    environment.pop("PYTHONPATH", None)
    environment.pop("PYTHONHOME", None)
    tests = Path(__file__).resolve().parents[1] / "bindings/python/tests/test_client.py"
    with tempfile.TemporaryDirectory(prefix="romedb-wheel-check-") as temporary:
        directory = Path(temporary)
        venv = directory / "venv"

        def run(command):
            subprocess.run(command, cwd=directory, env=environment, check=True)

        run([uv, "venv", "--python", python, str(venv)])
        executable = venv / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
        run([uv, "pip", "install", "--python", str(executable), "--no-index", str(wheel)])
        run([str(executable), "-I", str(tests), "-v"])


if __name__ == "__main__":
    main()
