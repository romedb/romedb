#!/usr/bin/env python3
"""Build native language consumers outside the checkout; no registry publication."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
LIBRARY = Path(os.environ.get("ROMEDB_LIBRARY", ROOT / "target/debug/libromedb_c.so")).resolve()
env = dict(os.environ)
env["LD_LIBRARY_PATH"] = str(LIBRARY.parent) + ":" + env.get("LD_LIBRARY_PATH", "")
env["CGO_ENABLED"] = "1"
env["CGO_LDFLAGS"] = "-L" + str(LIBRARY.parent)
env["DOTNET_CLI_TELEMETRY_OPTOUT"] = "1"


def run(args, cwd):
    subprocess.run(args, cwd=cwd, env=env, check=True, timeout=180)


with tempfile.TemporaryDirectory(prefix="romedb-native-packages-") as directory:
    base = Path(directory)
    for language in ["go", "php", "swift"]:
        shutil.copytree(ROOT / "romedb/bindings" / language, base / language,
                        ignore=shutil.ignore_patterns(".build", ".swiftpm", "vendor", "node_modules"))

    php = base / "php-consumer.php"
    php.write_text('''<?php
require __DIR__.'/php/src/RomeDB.php';
$db = new RomeDB\\Database(':memory:', $argv[1]);
try {
    $r = $db->execute('SELECT $n', ['$n'=>RomeDB\\Value::integer(PHP_INT_MAX)]);
    if ($r['rows'][0][0]['value'] !== (string)PHP_INT_MAX) throw new RuntimeException('PHP packaging');
} finally { $db->close(); }
echo "Isolated PHP consumer passed\\n";
''')
    run(["php", "-d", "ffi.enable=1", str(php), str(LIBRARY)], base)

    go = base / "go-consumer"
    go.mkdir()
    (go / "go.mod").write_text('''module example.com/romedb-consumer
go 1.22
require github.com/romedb/romedb/romedb/bindings/go/v2 v2.0.0
replace github.com/romedb/romedb/romedb/bindings/go/v2 => ../go
''')
    (go / "main.go").write_text('''package main
import ("fmt"; fdb "github.com/romedb/romedb/romedb/bindings/go/v2")
func main(){db,err:=fdb.Open(":memory:");if err!=nil{panic(err)};defer db.Close()
r,err:=db.Execute("SELECT $n",map[string]fdb.Value{"$n":fdb.Integer(9223372036854775807)},-1)
if err!=nil{panic(err)};n,err:=r.Rows[0][0].Int64();if err!=nil||n!=9223372036854775807{panic("Go packaging")}
fmt.Println("Isolated Go consumer passed")}
''')
    run(["go", "run", "."], go)

    swift = base / "swift-consumer"
    (swift / "Sources/Smoke").mkdir(parents=True)
    (swift / "Package.swift").write_text('''// swift-tools-version: 5.9
import PackageDescription
let package = Package(name:"Smoke", dependencies:[.package(path:"../swift")],
    targets:[.executableTarget(name:"Smoke", dependencies:[.product(name:"RomeDB",package:"swift")])])
''')
    (swift / "Sources/Smoke/main.swift").write_text('''import RomeDB
let db = try Database(path:":memory:")
defer { try? db.close() }
let r = try db.execute("SELECT $n",parameters:["$n":.integer(Int64.max)])
let rows = r["rows"] as! [[[String:Any]]]
precondition(rows[0][0]["value"] as? String == String(Int64.max))
print("Isolated Swift consumer passed")
''')
    run(["swift", "run", "-j", "2", "-Xlinker", "-L" + str(LIBRARY.parent),
         "-Xlinker", "-rpath", "-Xlinker", str(LIBRARY.parent), "Smoke"], swift)

    feed = base / "feed"
    run(["dotnet", "pack", str(ROOT / "romedb/bindings/csharp/RomeDB/RomeDB.csproj"),
         "-c", "Debug", "-o", str(feed)], base)
    dotnet = base / "dotnet-consumer"
    dotnet.mkdir()
    (dotnet / "Consumer.csproj").write_text('''<Project Sdk="Microsoft.NET.Sdk">
<PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net8.0</TargetFramework></PropertyGroup>
<ItemGroup><PackageReference Include="RomeDB.Embedded" Version="2.0.0" /></ItemGroup></Project>''')
    (dotnet / "NuGet.Config").write_text(f'''<configuration><packageSources><clear/>
<add key="local" value="{feed}"/></packageSources></configuration>''')
    (dotnet / "Program.cs").write_text('''using RomeDB; using System.Text.Json.Nodes;
using var db = new Database(":memory:");
var r = db.Execute("SELECT $n",new JsonObject{["$n"]=Value.Integer(long.MaxValue)});
if(r["rows"]![0]![0]!["value"]!.GetValue<string>()!=long.MaxValue.ToString())throw new System.Exception("NuGet packaging");
System.Console.WriteLine("Isolated NuGet consumer passed");
''')
    # Isolate the cache so a prior same-version development package cannot mask changes.
    env["NUGET_PACKAGES"] = str(base / "nuget-cache")
    run(["dotnet", "run", "--project", str(dotnet / "Consumer.csproj")], dotnet)

print("All four isolated native package consumers passed")
