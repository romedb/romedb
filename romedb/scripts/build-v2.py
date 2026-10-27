#!/usr/bin/env python3
"""Build a Linux x64 V2 bundle. Publication is a separate, verified release step."""
import argparse
import hashlib
import gzip
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile
from urllib.parse import quote, unquote, urlsplit, urlunsplit
from shipping_policy import PROFILE, POLICY, build_environment, check_artifact, check_profile, check_security_review

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("output", type=Path)
parser.add_argument("--development", action="store_true", help="Allow a worktree source snapshot; never publication eligible")
args = parser.parse_args()
check_profile((ROOT / "Cargo.toml").read_text())
build_env = build_environment()
out = args.output.resolve()
if out.is_relative_to(ROOT) and not out.is_relative_to(ROOT / "dist"):
    raise SystemExit("Place bundles outside the checkout or under ignored dist/")


def run(command, cwd=ROOT, **kwargs):
    kwargs.setdefault("env", build_env)
    subprocess.run(command, cwd=cwd, check=True, **kwargs)


def capture(command, cwd=ROOT):
    return subprocess.check_output(command, cwd=cwd, env=build_env, text=True).strip()


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def copy(source, target):
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, target)


def bundled_markdown(source, destination, destinations, source_commit):
    # These maintained documents use inline Markdown links. Preserve labels,
    # optional titles and angle brackets while relocating only local targets.
    def rewrite(match):
        original = match[2]
        angled = original.startswith("<")
        url = urlsplit(original[1:-1] if angled else original)
        if url.scheme or url.netloc or not url.path:
            return match[0]
        target = (source.parent / unquote(url.path)).resolve()
        if not target.is_relative_to(ROOT) or not target.is_file():
            raise ValueError(f"Missing or external documentation target in {source}: {original}")
        if target in destinations:
            path = quote(Path(os.path.relpath(destinations[target], destination.parent)).as_posix())
        else:
            path = f"https://github.com/romedb/romedb/blob/{source_commit}/" + quote(target.relative_to(ROOT).as_posix())
        rewritten = urlunsplit(("", "", path, url.query, url.fragment))
        if angled:
            rewritten = "<" + rewritten + ">"
        return match[1] + rewritten + match[3]

    return re.sub(r'''(\]\()(<[^>\n]+>|[^()\s]+)([ \t]*(?:"[^"\n]*"|'[^'\n]*')?[ \t]*\))''',
                  rewrite, source.read_text())


def archive(path, files):
    # All source paths are relative to the checkout. Keep package symlinks as
    # links; never follow them into unrelated directories.
    with path.open("wb") as stream, gzip.GzipFile(filename="", mode="wb", fileobj=stream, mtime=0) as compressed, tarfile.open(fileobj=compressed, mode="w", dereference=False) as tar:
        for source, name in sorted(files, key=lambda item: item[1]):
            info = tar.gettarinfo(str(source), arcname=name)
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.mtime = 0
            if info.isfile():
                with source.open("rb") as stream:
                    tar.addfile(info, stream)
            else:
                tar.addfile(info)


if platform.system() != "Linux" or platform.machine() != "x86_64":
    raise SystemExit("This release builder is qualified for Linux x64")
run(["python3", "romedb/scripts/check-release-versions.py"])
run(["python3", "romedb/scripts/check-runtime-notices.py"])
check_security_review(json.loads((ROOT / "romedb/docs/dependency-security-rustsec.json").read_text()), sha(ROOT / "Cargo.lock"))
status = capture(["git", "status", "--porcelain"])
source_commit = capture(["git", "rev-parse", "HEAD"])
if status and not args.development:
    raise SystemExit("Final candidates require a clean committed source tree; use --development for a nonpublishable rehearsal")
release = json.loads((ROOT / "romedb/release.json").read_text())
version = release["version"]
out.mkdir(parents=True, exist_ok=False)
for directory in ["bin", "lib", "include", "packages", "notices", "evidence"]:
    (out / directory).mkdir()

# A stale notice bundle must stop packaging, not get silently regenerated after
# the source identity was selected. Preparation is a separate checked operation.
for short, bundled in [("node", "bindings/node/THIRD_PARTY_CRATE_NOTICES.md"),
                       ("python", "bindings/python/THIRD_PARTY_CRATE_NOTICES.md"),
                       ("c", "bindings/c/THIRD_PARTY_CRATE_NOTICES.md"),
                       ("cli", "docs/cli-crate-notices-linux-x64.md")]:
    inventory = f"romedb/docs/{short}-dependencies-linux-x64.json"
    audit = f"romedb/docs/{short}-crate-notices-linux-x64.json"
    run(["python3", "romedb/scripts/bundle-crate-notices.py", inventory, audit, "romedb/" + bundled,
         "--supplements", "romedb/docs/notice-source-supplements.json", "--check", "--require-complete"])
    copy(ROOT / inventory, out / "evidence" / Path(inventory).name)
    copy(ROOT / audit, out / "evidence" / Path(audit).name)
    copy(ROOT / "romedb" / bundled, out / "notices" / f"{short}-CRATE_NOTICES.md")

build_command = ["cargo", "build", "--locked", "--profile", PROFILE, "--message-format=json-render-diagnostics",
                 "-p", "romedb-cli", "-p", "romedb-node", "-p", "romedb-c", "-p", "romedb-python"]
with (out / "evidence/cargo-build.jsonl").open("w") as log:
    run(build_command, stdout=log)
target_base = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
target = (target_base if target_base.is_absolute() else ROOT / target_base).resolve() / PROFILE
cargo_artifacts = {}
for line in (out / "evidence/cargo-build.jsonl").read_text().splitlines():
    message = json.loads(line)
    if message.get("reason") == "compiler-artifact" and message["target"]["name"] in {"romedb-cli", "romedb_node", "romedb_c", "_native"}:
        check_artifact(message["profile"])
        artifact = Path(message["executable"] or next(name for name in message["filenames"] if name.endswith(".so")))
        if artifact.parent != target:
            raise ValueError(f"Unexpected native artifact directory: {artifact}")
        cargo_artifacts[message["target"]["name"]] = {"profile": message["profile"], "path": artifact.relative_to(target.parent).as_posix(), "sha256": sha(artifact)}
if len(cargo_artifacts) != 4:
    raise ValueError("Cargo did not report all four native artifacts")
copy(target / "romedb-cli", out / "bin/romedb-cli")
copy(target / "libromedb_c.so", out / "lib/libromedb_c.so")
copy(ROOT / "romedb/bindings/c/include/romedb.h", out / "include/romedb.h")
copy(target / "libromedb_node.so", ROOT / "romedb/bindings/node/romedb.node")
# Strip only distributed copies, retaining full Cargo artifacts for diagnosis.
run(["strip", "--strip-debug", str(out / "bin/romedb-cli"), str(out / "lib/libromedb_c.so"),
     str(ROOT / "romedb/bindings/node/romedb.node")])
copy(out / "bin/romedb-cli", out / "bin/romedb")
packed = json.loads(capture(["pnpm", "pack", "--json", "--pack-destination", str(out / "packages")], ROOT / "romedb/bindings/node"))
node = Path(packed["filename"])
if not node.is_absolute():
    node = out / "packages" / node
if node.resolve().parent != out / "packages" or not node.is_file():
    raise ValueError("pnpm returned an invalid package location")
python_command = ["maturin", "build", "--locked", "--profile", PROFILE, "--strip", "--manifest-path", "romedb/bindings/python/Cargo.toml", "--out", str(out / "packages")]
run(python_command)
run(["dotnet", "pack", "romedb/bindings/csharp/RomeDB/RomeDB.csproj", "-c", "Release", "-o", str(out / "packages")],
    env=dict(build_env, DOTNET_CLI_TELEMETRY_OPTOUT="1"))

for language in ["php", "swift", "go"]:
    package = ROOT / "romedb/bindings" / language
    excluded = {".build", ".swiftpm", "node_modules", "vendor", "__pycache__"}
    files = [(p, f"romedb-{language}-{version}/" + p.relative_to(package).as_posix())
             for p in package.rglob("*") if (p.is_file() or p.is_symlink())
             and not excluded.intersection(p.relative_to(package).parts)]
    files.append((ROOT / "romedb/bindings/fixtures/native-client.json", f"romedb-{language}-{version}/testdata/native-client.json"))
    archive(out / "packages" / f"romedb-{language}-{version}.tar.gz", files)

documents = [("romedb/docs/v2-release-quickstart.md", "README.md"),
                     ("romedb/docs/backup-restore.md", "BACKUP.md"),
                     ("romedb/docs/deployment.md", "DEPLOYMENT.md"),
                     ("romedb/docs/operations.md", "OPERATIONS.md"),
                     ("romedb/docs/sqlite-adoption.md", "SQLITE-ADOPTION.md"),
                     ("romedb/docs/sqlite-notice.md", "notices/SQLITE.md"),
                     ("romedb/docs/dependency-security.md", "evidence/dependency-security.md"),
                     ("romedb/docs/dependency-security-rustsec.json", "evidence/dependency-security-rustsec.json"),
                     ("romedb/docs/dependency-security-native.json", "evidence/dependency-security-native.json"),
                     ("romedb/docs/native-language-clients.md", "NATIVE-CLIENTS.md"),
                     ("romedb/docs/production-build.md", "BUILD-POLICY.md"),
                     ("romedb/UPSTREAM.md", "UPSTREAM.md"), ("LICENSE.md", "LICENSE.md"),
                     ("romedb/bindings/c/RUST-LIBRARY-NOTICES.html", "notices/RUST-LIBRARY-NOTICES.html"),
                     ("romedb/docs/rust-runtime-notices.json", "evidence/rust-runtime-notices.json"),
                     ("romedb/bindings/node/THIRD_PARTY_NOTICES.md", "notices/THIRD_PARTY_NOTICES.md")]
destinations = {(ROOT / source).resolve(): Path(name) for source, name in documents}
for source, name in documents:
    copy(ROOT / source, out / name)
    if Path(source).suffix == ".md":
        (out / name).write_text(bundled_markdown(ROOT / source, Path(name), destinations, source_commit))

if capture(["git", "rev-parse", "HEAD"]) != source_commit:
    raise ValueError("Source commit changed during build")
if not args.development and capture(["git", "status", "--porcelain"]):
    raise ValueError("Final source tree changed during build")
if args.development:
    names = subprocess.check_output(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=ROOT).decode().split("\0")
    files = [(ROOT / n, "romedb-source/" + n) for n in set(names) if n and ((ROOT / n).exists() or (ROOT / n).is_symlink())]
    archive(out / "romedb-source.tar.gz", files)
else:
    run(["git", "archive", "--format=tar.gz", "--prefix=romedb-source/", "-o", str(out / "romedb-source.tar.gz"), source_commit])

manifest = {
    "version": version, "sourceCommit": source_commit, "sourceSnapshot": args.development,
    "sourceStatus": status.splitlines() if args.development else [],
    "sourceArchiveSha256": sha(out / "romedb-source.tar.gz"),
    "lockfileSha256": sha(ROOT / "Cargo.lock"), "engineBase": release["engineBase"],
    "buildProfile": PROFILE, "rustProfilePolicy": POLICY, "csharpConfiguration": "Release",
    "cargoArtifacts": cargo_artifacts, "cargoBuildCommand": build_command, "pythonBuildCommand": python_command,
    "sourceCargoTomlSha256": sha(ROOT / "Cargo.toml"), "sourceCargoConfigSha256": sha(ROOT / ".cargo/config.toml"),
    "distributedSymbols": "native copies stripped; Cargo originals retained for diagnostics",
    "platform": platform.platform(), "architecture": platform.machine(), "libc": platform.libc_ver(),
    "rust": capture(["rustc", "-vV"]), "node": capture(["node", "--version"]),
    "pnpm": capture(["pnpm", "--version"]), "maturin": capture(["maturin", "--version"]),
    "dotnet": capture(["dotnet", "--version"]), "clients": release["clients"],
    "publication": "local candidate only", "publicationEligible": False,
    "requiredBeforePublication": ["exact artifact qualification", "clean committed source", "release evidence and checksums"],
}
(out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
files = sorted(p for p in out.rglob("*") if p.is_file())
(out / "SHA256SUMS").write_text("".join(sha(p) + "  " + p.relative_to(out).as_posix() + "\n" for p in files))
print(out)
