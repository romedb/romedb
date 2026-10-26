#!/usr/bin/env python3
"""Verify exact local V2 bundle artifacts; write evidence outside the bundle."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
from shipping_policy import check_manifest, check_profile, check_artifact, check_security_review
import upgrade_matrix

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("bundle", type=Path)
parser.add_argument("evidence", type=Path)
parser.add_argument("--node", action="append", required=True, help="Node executable; repeat for each qualified runtime")
parser.add_argument("--python", action="append", required=True, help="Python executable; repeat for each qualified runtime")
parser.add_argument("--uv", default="uv")
parser.add_argument("--previous-package", action="append", default=[], metavar="VERSION=PATH",
                    help="Immutable published Node package of an earlier release; repeat for every version upgrade_matrix.py requires")
args = parser.parse_args()
bundle = args.bundle.resolve()
evidence = args.evidence.resolve()
if evidence.is_relative_to(bundle):
    raise SystemExit("Evidence must be outside the checksummed bundle")
evidence.mkdir(parents=True, exist_ok=False)


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


checksums = {}
for line in (bundle / "SHA256SUMS").read_text().splitlines():
    checksum, name = line.split("  ", 1)
    path = (bundle / name).resolve()
    if not path.is_relative_to(bundle) or name in checksums or sha(path) != checksum:
        raise ValueError(f"Invalid or changed bundle file: {name}")
    checksums[name] = checksum
actual = {p.relative_to(bundle).as_posix() for p in bundle.rglob("*") if p.is_file() and p != bundle / "SHA256SUMS"}
assert actual == checksums.keys(), "Unlisted or missing bundle files"
manifest = json.loads((bundle / "manifest.json").read_text())
check_manifest(manifest)
check_security_review(json.loads((bundle / "evidence/dependency-security-rustsec.json").read_text()), manifest["lockfileSha256"])
receipts = {}
for line in (bundle / "evidence/cargo-build.jsonl").read_text().splitlines():
    receipt = json.loads(line)
    if receipt.get("reason") == "compiler-artifact" and receipt["target"]["name"] in manifest["cargoArtifacts"]:
        check_artifact(receipt["profile"])
        receipts[receipt["target"]["name"]] = receipt["profile"]
assert receipts == {name: value["profile"] for name, value in manifest["cargoArtifacts"].items()}, "Manifest does not match Cargo build receipts"
with tarfile.open(bundle / "romedb-source.tar.gz") as source:
    source_cargo = source.extractfile("romedb-source/Cargo.toml").read()
    check_profile(source_cargo.decode())
    assert hashlib.sha256(source_cargo).hexdigest() == manifest["sourceCargoTomlSha256"]
    assert hashlib.sha256(source.extractfile("romedb-source/.cargo/config.toml").read()).hexdigest() == manifest["sourceCargoConfigSha256"]
    assert hashlib.sha256(source.extractfile("romedb-source/Cargo.lock").read()).hexdigest() == manifest["lockfileSha256"]
version = manifest["version"]
release = json.loads((ROOT / "romedb/release.json").read_text())
assert version == release["version"]
node_versions = [subprocess.check_output([node, "-p", "process.versions.node"], text=True).strip() for node in args.node]
python_versions = [subprocess.check_output([python, "-c", "import platform;print(platform.python_version())"], text=True).strip() for python in args.python]
assert {int(v.split('.')[0]) for v in node_versions} == set(release["requiredNodeMajors"]), "Incomplete Node runtime matrix"
assert {'.'.join(v.split('.')[:2]) for v in python_versions} == set(release["requiredPythonMinors"]), "Incomplete Python runtime matrix"
assert sha(bundle / "romedb-source.tar.gz") == manifest["sourceArchiveSha256"]
packages = upgrade_matrix.previous_packages(args.previous_package, version)
results = []


def run(name, command, cwd=ROOT, env=None, input=None):
    with (evidence / f"{name}.log").open("w") as log:
        subprocess.run(command, cwd=cwd, env=env, input=input, text=True,
                       stdout=log, stderr=subprocess.STDOUT, check=True)
    results.append(name)
    print(name + " passed", flush=True)


base_env = dict(os.environ)
base_env["LD_LIBRARY_PATH"] = str(bundle / "lib") + ":" + base_env.get("LD_LIBRARY_PATH", "")
base_env["ROMEDB_LIBRARY"] = str(bundle / "lib/libromedb_c.so")
base_env["CGO_ENABLED"] = "1"
base_env["CGO_LDFLAGS"] = "-L" + str(bundle / "lib")
base_env.pop("ROMEDB_FIXTURE", None)
run("c-abi", ["python3", "romedb/scripts/check-c-abi.py", str(bundle / "lib/libromedb_c.so")], env=base_env)
run("cli", [str(bundle / "bin/romedb-cli"), "--script"], input="SELECT geo::cell(geo::point(100,13),7);\n")
cli = json.loads((evidence / "cli.log").read_text())
assert cli["rows"] == [[{"type": "String", "value": "87658b314ffffff"}]]
cli_v22 = """INSERT INTO writers {id:writers:alice,name:'Alice'};
CREATE TABLE articles;
DEFINE FIELD stamp ON articles TYPE integer DEFAULT 7 READONLY;
DEFINE FIELD total ON articles TYPE integer VALUE (n*2);
INSERT INTO articles {id:articles:a,title:'Typed SQL',author:writers:alice,tags:[1,2],n:1};
CREATE INDEX articles_author ON articles(author);
CREATE UNIQUE INDEX article_pair ON articles(n,title);
CREATE SEARCH INDEX article_tags ON articles(tags) USING ARRAY;
SELECT id,title,author.* AS writer FROM articles WHERE author=type::record('writers','alice') ORDER BY id;
SELECT a.id,a.title,a.author.* AS writer FROM articles a WHERE a.author=type::record('writers','alice') ORDER BY a.id LIMIT 20;
UPDATE articles:a PATCH [{op:'add',path:'/tags/-',value:3}] RETURNING n,total,tags;
INSERT INTO articles(n,title) VALUES(1,'Typed SQL') ON CONFLICT(n,title) DO UPDATE SET n=2 RETURNING n,total;
SELECT n FROM articles WHERE array::contains(tags,3);
SELECT n FROM articles WHERE (n,tags) IN(SELECT n,tags FROM articles);
WITH RECURSIVE r(n) AS (SELECT n FROM articles UNION ALL SELECT n+1 FROM r WHERE n<4) SELECT n FROM r;
"""
run("cli-v22", [str(bundle / "bin/romedb-cli"), "--script"], input=cli_v22)
cli_rows = [json.loads(line) for line in (evidence / "cli-v22.log").read_text().splitlines()]
assert len(cli_rows) == 15 and all("error" not in result for result in cli_rows)
def integer(value):
    # The CLI serializes tagged values through serde_json: integers are JSON
    # numbers, unlike the string-encoded integers of the C ABI fixture protocol.
    return {"type": "Integer", "value": value}
writer = {"type": "Object", "value": {"id": {"type": "Record", "value": {"table": "writers", "key": {"String": "alice"}}}, "name": {"type": "String", "value": "Alice"}}}
assert cli_rows[8]["rows"] == cli_rows[9]["rows"]
assert cli_rows[8]["columns"] == ["id", "title", "writer"]
assert cli_rows[8]["rows"][0][2] == writer
assert cli_rows[10]["rows"] == [[integer(1), integer(2), {"type": "Array", "value": [integer(1), integer(2), integer(3)]}]]
assert cli_rows[11]["rows"] == [[integer(2), integer(4)]]
assert cli_rows[12]["rows"] == cli_rows[13]["rows"] == [[integer(2)]]
assert cli_rows[14]["rows"] == [[integer(2)], [integer(3)], [integer(4)]]
if True:  # graph checks, present since the first RomeDB release
    cli_graph = """CREATE TABLE people;
CREATE EDGE TABLE follows FROM people TO people;
INSERT INTO people:a {name:'A'};
INSERT INTO people:b {name:'B'};
RELATE people:a -> follows:ab -> people:b;
SELECT g.node, g.depth, g.edges FROM GRAPH(people:a -> follows -> people DEPTH 1 TO 2 MODE REACHABLE WITH PATH) g;
EXPLAIN QUERY PLAN SELECT g.node FROM GRAPH(people:a -> follows -> people) g;
"""
    run("cli-graph", [str(bundle / "bin/romedb-cli"), "--script"], input=cli_graph)
    graph_rows = [json.loads(line) for line in (evidence / "cli-graph.log").read_text().splitlines()]
    assert len(graph_rows) == 7 and all("error" not in result for result in graph_rows)
    record = lambda table, key: {"type": "Record", "value": {"table": table, "key": {"String": key}}}
    assert graph_rows[5]["rows"] == [[record("people", "b"), integer(1), {"type": "Array", "value": [record("follows", "ab")]}]]
    assert any("GRAPH HOP 1 follows SEARCH USING COVERING INDEX" in json.dumps(row) for row in graph_rows[6]["rows"])
if True:  # spatial checks, present since the first RomeDB release
    cli_spatial = """CREATE TABLE places;
INSERT INTO places:a {location:ST_Point(106.70, 10.80), open:true};
INSERT INTO places:b {location:ST_Point(106.71, 10.80), open:false};
INSERT INTO places:c {location:ST_Point(106.75, 10.85), open:true};
CREATE SEARCH INDEX places_location ON places(location) USING SPATIAL;
SELECT id FROM places WHERE ST_DistanceSphere(location, ST_Point(106.7, 10.8)) IS NOT NULL AND open = true ORDER BY ST_DistanceSphere(location, ST_Point(106.7, 10.8)), id LIMIT 2;
CREATE TABLE zones;
INSERT INTO zones:north {shape:ST_MakeEnvelope(106.74, 10.84, 106.76, 10.86)};
CREATE SEARCH INDEX zones_shape ON zones(shape) USING SPATIAL WITH (geometry = 'region');
SELECT id FROM zones WHERE ST_Covers(shape, ST_Point(106.75, 10.85));
SELECT geo::cell_resolution(geo::cell(ST_Point(106.7, 10.8), 9)), ST_AsGeoJSON(ST_Point(1.5, 2));
EXPLAIN QUERY PLAN SELECT id FROM places WHERE ST_DWithin(location, ST_Point(106.7, 10.8), 1000);
"""
    run("cli-spatial", [str(bundle / "bin/romedb-cli"), "--script"], input=cli_spatial)
    spatial_rows = [json.loads(line) for line in (evidence / "cli-spatial.log").read_text().splitlines()]
    assert len(spatial_rows) == 12 and all("error" not in result for result in spatial_rows)
    record = lambda table, key: {"type": "Record", "value": {"table": table, "key": {"String": key}}}
    assert spatial_rows[5]["rows"] == [[record("places", "a")], [record("places", "c")]]
    assert spatial_rows[9]["rows"] == [[record("zones", "north")]]
    assert spatial_rows[10]["rows"][0][0] == integer(9)
    assert '{\\"type\\":\\"Point\\",\\"coordinates\\":[1.5,2.0]}' in json.dumps(spatial_rows[10]["rows"][0][1])
    assert "places_location" in json.dumps(spatial_rows[11]["rows"])
run("sqlite-adoption", ["python3", "romedb/scripts/check-sqlite-adoption.py",
                        str(bundle / "bin/romedb-cli"), str(evidence / "sqlite-adoption.json")], env=base_env)
sqlite_adoption = json.loads((evidence / "sqlite-adoption.json").read_text())
assert sqlite_adoption["cliSha256"] == checksums["bin/romedb-cli"]
for name, file in [("cli-elf", bundle / "bin/romedb-cli"), ("c-elf", bundle / "lib/libromedb_c.so")]:
    run(name, ["python3", "romedb/scripts/inspect-node-elf.py", str(file), str(evidence / f"{name}.json")])

for index, node in enumerate(args.node):
    env = dict(base_env, ROMEDB_PACKAGE_TARBALL=str(bundle / "packages" / f"romedb-node-{version}.tgz"))
    # pnpm's child Node process must use this runtime too.
    env["PATH"] = str(Path(node).resolve().parent) + ":" + env.get("PATH", "")
    run(f"node-{index}", [node, "romedb/scripts/check-node-package.cjs"], env=env)
wheel, = (bundle / "packages").glob("*.whl")
for index, python in enumerate(args.python):
    run(f"python-{index}", ["python3", "romedb/scripts/check-python-wheel.py", str(wheel), "--python", python, "--uv", args.uv])

with tempfile.TemporaryDirectory(prefix="romedb-v2-bundle-") as temp:
    temporary = Path(temp)
    for language in ["php", "go", "swift"]:
        with tarfile.open(bundle / "packages" / f"romedb-{language}-{version}.tar.gz") as tar:
            tar.extractall(temporary, filter="data")
    php = temporary / f"romedb-php-{version}"
    run("php-package", ["php", "-d", "ffi.enable=1", "tests/smoke.php"], cwd=php, env=base_env)
    go = temporary / f"romedb-go-{version}"
    run("go-package", ["go", "test", "-race", "-v", "./..."], cwd=go, env=base_env)
    swift = temporary / f"romedb-swift-{version}"
    run("swift-package", ["swift", "test", "-j", "2", "-Xlinker", "-L" + str(bundle / "lib"),
                          "-Xlinker", "-rpath", "-Xlinker", str(bundle / "lib")], cwd=swift, env=base_env)
    consumer = temporary / "node-consumer"
    consumer.mkdir()
    (consumer / "package.json").write_text('{"name":"v2-upgrade-check","private":true}')
    run("node-upgrade-install", ["pnpm", "add", "--offline", "--ignore-scripts", str(bundle / "packages" / f"romedb-node-{version}.tgz")], cwd=consumer)
    installed_node = consumer / "node_modules/@romedb/node"

    def ownership(index, node):
        env = dict(base_env, ROMEDB_OWNERSHIP_PACKAGE=str(installed_node))
        run(f"node-ownership-{index}", [node, "--test", "romedb/bindings/node/ownership.test.cjs"], env=env)

    upgrade_matrix.run_upgrades(run, ROOT, version, installed_node, packages, args.node, evidence,
                                before_node=ownership)

    # Test the shipped .nupkg, never a previously cached same-version package.
    dotnet = temporary / "dotnet"
    dotnet.mkdir()
    (dotnet / "Tests.csproj").write_text(f'''<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup>
<OutputType>Exe</OutputType><TargetFramework>net8.0</TargetFramework><ImplicitUsings>enable</ImplicitUsings><Nullable>enable</Nullable>
</PropertyGroup><ItemGroup><PackageReference Include="RomeDB.Embedded" Version="{version}"/></ItemGroup></Project>''')
    (dotnet / "Program.cs").write_bytes((ROOT / "romedb/bindings/csharp/Tests/Program.cs").read_bytes())
    (dotnet / "NuGet.Config").write_text(f'<configuration><packageSources><clear/><add key="bundle" value="{bundle / "packages"}"/></packageSources></configuration>')
    nuget_cache = temporary / "nuget-cache"
    # Only the RomeDB package must come from the bundle. SDK targeting packs are
    # not RomeDB artifacts; newer SDKs restore them through NuGet even when the
    # source list is cleared, so seed them from the host cache to keep the
    # isolated cache usable without opening a network source.
    global_cache = Path(os.environ.get("NUGET_PACKAGES", Path.home() / ".nuget" / "packages"))
    for pack in ["microsoft.netcore.app.ref", "microsoft.aspnetcore.app.ref", "microsoft.netcore.app.host.linux-x64"]:
        source = global_cache / pack
        if source.is_dir():
            shutil.copytree(source, nuget_cache / pack, dirs_exist_ok=True)
    env = dict(base_env, NUGET_PACKAGES=str(nuget_cache), DOTNET_CLI_TELEMETRY_OPTOUT="1")
    run("nuget-package", ["dotnet", "run", "--", str(ROOT / "romedb/bindings/fixtures/native-client.json")], cwd=dotnet, env=env)

report = {"version": version, "buildProfile": manifest["buildProfile"], "rustProfilePolicy": manifest["rustProfilePolicy"], "bundleManifestSha256": sha(bundle / "manifest.json"),
          "bundleChecksumsSha256": sha(bundle / "SHA256SUMS"), "passed": results, "nodeVersions": node_versions, "pythonVersions": python_versions,
          "sqliteAdoption": sqlite_adoption,
          "scope": "Exact Linux native artifacts on the recorded host; source/CI/publication remain separate gates"}
(evidence / "verification.json").write_text(json.dumps(report, indent=2) + "\n")
print(evidence)
