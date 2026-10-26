#!/usr/bin/env python3
"""Build a Linux x64 CLI candidate for Cloud 0.4.0 without replacing V2 releases.

Requires a clean committed engine checkout and prepared CLI notice records.
Publication and acceptance against the Cloud service remain separate steps.
"""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tomllib

from shipping_policy import PROFILE, POLICY, build_environment, check_artifact, check_profile

ROOT = Path(__file__).resolve().parents[2]


def sha(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="New directory outside the checkout or under dist/")
    args = parser.parse_args()
    out = args.output.resolve()
    if out.is_relative_to(ROOT) and not out.is_relative_to(ROOT / "dist"):
        parser.error("output must be outside the checkout or under dist/")
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        parser.error("only Linux x64 is qualified")
    check_profile((ROOT / "Cargo.toml").read_text())
    env = build_environment()
    env.setdefault("CARGO_BUILD_JOBS", "2")

    def capture(*command):
        return subprocess.check_output(command, cwd=ROOT, env=env, text=True).strip()

    def run(*command, **kwargs):
        return subprocess.run(command, cwd=ROOT, env=env, check=True, **kwargs)

    if capture("git", "status", "--porcelain", "--untracked-files=all"):
        raise ValueError("CLI candidate requires clean committed source")
    commit = capture("git", "rev-parse", "HEAD")
    epoch = str(int(env.get("SOURCE_DATE_EPOCH") or capture("git", "show", "-s", "--format=%ct", commit)))
    if int(epoch) < 0:
        raise ValueError("SOURCE_DATE_EPOCH must be nonnegative")
    env["SOURCE_DATE_EPOCH"] = epoch
    cli = tomllib.loads((ROOT / "romedb/cli/Cargo.toml").read_text())["package"]
    inventory = "romedb/docs/cli-dependencies-linux-x64.json"
    audit = "romedb/docs/cli-crate-notices-linux-x64.json"
    notices = "romedb/docs/cli-crate-notices-linux-x64.md"
    # Verify the actual CLI graph, rather than accepting only a matching lock hash.
    graph_env = dict(env, ROMEDB_INVENTORY_PACKAGE="romedb-cli")
    subprocess.run(["node", "romedb/scripts/inventory-node-dependencies.cjs", "x86_64-unknown-linux-gnu",
                    inventory, "--check"], cwd=ROOT, env=graph_env, check=True)
    run(sys.executable, "romedb/scripts/audit-crate-notices.py", inventory, audit, "--check")
    run(sys.executable, "romedb/scripts/bundle-crate-notices.py", inventory, audit, notices,
        "--supplements", "romedb/docs/notice-source-supplements.json", "--check", "--require-complete")
    run(sys.executable, "romedb/scripts/check-runtime-notices.py")
    out.mkdir(parents=True, exist_ok=False)
    for directory in ("bin", "notices", "evidence"):
        (out / directory).mkdir()
    command = ["cargo", "build", "--locked", "--offline", "--profile", PROFILE,
               "-p", "romedb-cli", "--message-format=json-render-diagnostics"]
    with (out / "evidence/cargo.jsonl").open("w") as stdout, (out / "evidence/cargo.log").open("w") as stderr:
        run(*command, stdout=stdout, stderr=stderr)
    artifacts = []
    for line in (out / "evidence/cargo.jsonl").read_text().splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message["target"]["name"] == "romedb-cli" and message.get("executable"):
            check_artifact(message["profile"])
            artifacts.append(message)
    if len(artifacts) != 1:
        raise ValueError("Cargo must report exactly one CLI executable")
    artifact = artifacts[0]
    original = Path(artifact["executable"])
    if original.parent.name != PROFILE:
        raise ValueError("CLI artifact is outside the shipping profile")
    binary = out / "bin/romedb-cli"
    shutil.copy2(original, binary)
    run("strip", "--strip-debug", str(binary))
    for script in ("check-cloud-cli.py", "check-cloud-import-cli.py"):
        with (out / "evidence" / (script + ".log")).open("w") as log:
            run(sys.executable, "romedb/scripts/" + script, str(binary), stdout=log, stderr=subprocess.STDOUT)
    for source, destination in (
        ("LICENSE.md", "LICENSE.md"), ("romedb/docs/cloud-cli.md", "CLOUD-CLI.md"),
        ("romedb/docs/cloud-query-protocol-v2.md", "cloud-query-protocol-v2.md"),
        (notices, "notices/CRATE-NOTICES.md"),
        ("romedb/bindings/node/THIRD_PARTY_NOTICES.md", "notices/THIRD_PARTY_NOTICES.md"),
        ("romedb/bindings/node/RUST-LIBRARY-NOTICES.html", "notices/RUST-LIBRARY-NOTICES.html"),
        (inventory, "evidence/dependencies.json"), (audit, "evidence/crate-notices.json"),
    ):
        shutil.copy2(ROOT / source, out / destination)
    (out / "README.md").write_text(
        "# RomeDB CLI for Cloud 0.4.0\n\n"
        f"Linux x64 candidate from engine commit `{commit}`. The embedded CLI version is {cli['version']}.\n"
        "This package supports Cloud 0.4.0 organization routing, readVersion 2, version-3 read journals, version-2 query journals and resumable imports.\n"
        "It does not replace the published embedded 2.1.0 client release.\n\n"
        "Run `bin/romedb-cli cloud --help`; see CLOUD-CLI.md for commands and recovery limits.\n"
        "Use with Cloud 0.4.0; the old 0.2 database routes are not supported by this client.\n"
        "Keep API keys in the environment. Recovery journals retain SQL and request identities.\n\n"
        "The binary uses the checked RomeDB production profile with assertions and overflow checks.\n"
        "License and notice texts are included. Verify SHA256SUMS before use.\n"
        "Candidate packaging does not establish hosted capacity or release availability.\n")
    run("git", "archive", "--format=tar.gz", "--output=" + str(out / "source.tar.gz"), commit)
    if capture("git", "rev-parse", "HEAD") != commit or capture("git", "status", "--porcelain", "--untracked-files=all"):
        raise ValueError("Source changed during CLI packaging")
    manifest = {
        "format": "romedb-cloud-cli-v1", "cloudCompatibility": "0.4.0", "cliVersion": cli["version"],
        "readProtocolVersion": 2, "readJournalVersion": 3, "queryJournalVersion": 2,
        "sourceCommit": commit, "sourceSnapshot": False, "sourceDateEpoch": int(epoch),
        "sourceArchiveSha256": sha(out / "source.tar.gz"), "lockfileSha256": sha(ROOT / "Cargo.lock"),
        "buildProfile": PROFILE, "rustProfilePolicy": POLICY, "cargoBuildCommand": command,
        "cargoArtifactProfile": artifact["profile"], "originalArtifactSha256": sha(original),
        "distributedArtifactSha256": sha(binary), "rust": capture("rustc", "-vV"),
        "platform": "x86_64-unknown-linux-gnu", "libc": platform.libc_ver(),
        "checks": ["locked CLI dependency/notice verification", "Rust runtime notices",
                   "synthetic organization/request recovery protocol", "synthetic resumable import protocol"],
        "publicationEligible": False,
        "remaining": ["packaged binary acceptance against Cloud runtime", "service release gates and artifact publication"],
    }
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    packaged = sorted(path for path in out.rglob("*") if path.is_file())
    (out / "SHA256SUMS").write_text("".join(f"{sha(path)}  {path.relative_to(out).as_posix()}\n" for path in packaged))
    archive = out / "romedb-cloud-cli-0.4.0-read-v2-linux-x64.tar.gz"
    with archive.open("wb") as stream, gzip.GzipFile(filename="", fileobj=stream, mode="wb", mtime=0) as zipped, tarfile.open(fileobj=zipped, mode="w") as tar:
        for path in sorted([*packaged, out / "SHA256SUMS"]):
            info = tar.gettarinfo(str(path), arcname=path.relative_to(out).as_posix())
            info.uid = info.gid = info.mtime = 0
            info.uname = info.gname = ""
            with path.open("rb") as source:
                tar.addfile(info, source)
    print(json.dumps({"archive": str(archive), "sha256": sha(archive), "sourceCommit": commit,
                      "binarySha256": sha(binary), "publicationEligible": False}))


if __name__ == "__main__":
    main()
