#!/usr/bin/env python3
"""Qualify SQLite adoption through the supplied exact installed RomeDB CLI."""
import argparse
from contextlib import closing
import hashlib
import json
from pathlib import Path
import platform
import sqlite3
import subprocess
import tempfile


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def verify(cli, evidence):
    cases = []
    importer_versions = set()

    def invoke(arguments, *, sql=None, succeeds=True):
        process = subprocess.run([str(cli), *map(str, arguments)], input=sql,
                                 capture_output=True, text=True, timeout=30)
        assert (process.returncode == 0) == succeeds, (
            arguments, process.returncode, process.stdout, process.stderr)
        return process

    def adoption(operation, source, destination=None, *, succeeds=True):
        arguments = ["sqlite", operation, source]
        if destination is not None:
            arguments.append(destination)
        process = invoke(arguments, succeeds=succeeds)
        report = json.loads(process.stdout)
        assert report["compatible"] is succeeds, report
        importer_versions.add(report["sqlite_version"])
        return report

    def query(path, sql, *, succeeds=True, error_code=None):
        result = invoke([path, "--script"], sql=sql + "\n", succeeds=succeeds)
        rows = [json.loads(line) for line in result.stdout.splitlines()]
        if succeeds:
            assert all("error" not in row for row in rows), rows
        if error_code is not None:
            assert rows[-1].get("error", {}).get("code") == error_code, rows
        return rows

    def scalar(row):
        return row["rows"][0][0]["value"]

    def checkpoint(row):
        assert row["columns"] == ["busy", "log", "checkpointed"], row
        assert row["rows"] == [[{"type": "Integer", "value": 0}] * 3], row

    def create(path, sql):
        with sqlite3.connect(path) as connection:
            connection.executescript(sql)
        connection.close()

    def readonly(path):
        return sqlite3.connect(path.as_uri() + "?mode=ro", uri=True)

    with tempfile.TemporaryDirectory(prefix="romedb-sqlite-artifact-") as temporary:
        root = Path(temporary)
        ordinary = root / "ordinary"
        ordinary.mkdir()
        source, destination = ordinary / "source.db", ordinary / "adopted.db"
        create(source, """
            CREATE TABLE parent(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
                                n INTEGER, r REAL, b BLOB, empty TEXT) STRICT;
            CREATE TABLE child(id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent(id) ON DELETE CASCADE);
            CREATE TABLE audit(event TEXT);
            CREATE VIEW names AS SELECT id,name FROM parent;
            CREATE INDEX parent_name ON parent(lower(name)) WHERE name IS NOT NULL;
            CREATE TRIGGER inserted AFTER INSERT ON parent BEGIN INSERT INTO audit VALUES(new.name); END;
            INSERT INTO parent VALUES(10,'deleted',NULL,NULL,NULL,NULL); DELETE FROM parent;
            INSERT INTO parent(name,n,r,b,empty) VALUES('Tân 雪',-9223372036854775808,1.25,x'00ff',NULL);
            INSERT INTO child VALUES(1,11);
            INSERT INTO parent(id,name) VALUES(100,'deleted high water');
            DELETE FROM parent WHERE id=100;
            PRAGMA user_version=17; PRAGMA application_id=42;
        """)
        original = source.read_bytes()
        source_sha = sha(source)
        adoption("check", source)
        assert source.read_bytes() == original
        adoption("import", source, destination)
        initial = query(destination, """
            SELECT id,name,n,r,b,empty FROM parent;
            SELECT name FROM names;
            PRAGMA user_version; PRAGMA application_id;
            PRAGMA wal_checkpoint(TRUNCATE);
        """)
        assert initial[0]["rows"] == [[
            {"type": "Integer", "value": 11}, {"type": "String", "value": "Tân 雪"},
            {"type": "Integer", "value": -(2 ** 63)}, {"type": "Number", "value": 1.25},
            {"type": "Binary", "value": [0, 255]}, {"type": "Null"},
        ]], initial
        assert scalar(initial[1]) == "Tân 雪"
        assert scalar(initial[2]) == 17 and scalar(initial[3]) == 42
        checkpoint(initial[4])
        # Enable FK enforcement in the same process as the failing operation.
        query(destination, "PRAGMA foreign_keys=ON; INSERT INTO child VALUES(2,999);", succeeds=False, error_code="FDB_CONSTRAINT")
        query(destination, "UPDATE parent SET n='invalid';", succeeds=False, error_code="FDB_CONSTRAINT")
        unchanged = query(destination, "SELECT n FROM parent; SELECT count(*) FROM child;")
        assert scalar(unchanged[0]) == -(2 ** 63) and scalar(unchanged[1]) == 1
        written = query(destination, """
            PRAGMA foreign_keys=ON;
            UPDATE parent SET name='updated',n=9223372036854775807;
            SELECT n FROM parent;
            INSERT INTO parent(name) VALUES('new');
            SELECT id FROM parent WHERE name='new';
            SELECT count(*) FROM audit;
            DELETE FROM parent WHERE id=11;
            SELECT count(*) FROM child;
            PRAGMA integrity_check;
            PRAGMA wal_checkpoint(TRUNCATE);
        """)
        assert scalar(written[2]) == 2 ** 63 - 1
        assert scalar(written[4]) == 101
        assert scalar(written[5]) == 4
        assert scalar(written[7]) == 0
        assert scalar(written[8]) == "ok"
        checkpoint(written[9])
        with closing(readonly(destination)) as connection:
            assert connection.execute("SELECT id,name FROM parent").fetchall() == [(101, "new")]
            assert connection.execute("SELECT count(*) FROM child").fetchone() == (0,)
            assert connection.execute("SELECT count(*) FROM audit").fetchone() == (4,)
            assert connection.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
            assert connection.execute("SELECT count(*) FROM sqlite_schema WHERE name='parent_name'").fetchone() == (1,)
        assert source.read_bytes() == original
        cases.append({"name": "ordinary-schema-values-read-write", "sourceSha256": source_sha,
                      "sourceUnchanged": True, "sqliteReadback": True,
                      "checks": ["strict", "foreign-keys", "cascade", "trigger", "view",
                                 "partial-expression-index", "autoincrement", "int64-min-max",
                                 "blob", "unicode", "float", "null", "header-pragmas"]})

        wal = root / "wal"
        wal.mkdir()
        source, destination = wal / "source.db", wal / "adopted.db"
        connection = sqlite3.connect(source)
        try:
            connection.executescript("""
                PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
                CREATE TABLE events(n INTEGER); INSERT INTO events VALUES(1);
                BEGIN IMMEDIATE; INSERT INTO events VALUES(2);
            """)
            original = source.read_bytes()
            wal_path = source.with_name(source.name + "-wal")
            original_wal = wal_path.read_bytes()
            assert original_wal
            adoption("check", source)
            adoption("import", source, destination)
            result = query(destination, "SELECT n FROM events; PRAGMA wal_checkpoint(TRUNCATE);")
            assert result[0]["rows"] == [[{"type": "Integer", "value": 1}]], result
            checkpoint(result[1])
            assert source.read_bytes() == original and wal_path.read_bytes() == original_wal
            with closing(readonly(destination)) as readback:
                assert readback.execute("SELECT n FROM events").fetchall() == [(1,)]
            cases.append({"name": "committed-wal-snapshot", "sourceUnchanged": True,
                          "walUnchanged": True, "uncommittedWriterExcluded": True})
        finally:
            connection.rollback()
            connection.close()

        unsupported = [
            ("without-rowid", "CREATE TABLE t(k TEXT PRIMARY KEY,v INT) WITHOUT ROWID", "WITHOUT ROWID"),
            ("generated-virtual", "CREATE TABLE t(n INT,x INT GENERATED ALWAYS AS(n+1) VIRTUAL)", "generated columns"),
            ("generated-stored", "CREATE TABLE t(n INT,x INT GENERATED ALWAYS AS(n+1) STORED)", "generated columns"),
            ("fts5", "CREATE VIRTUAL TABLE t USING fts5(body)", "virtual tables"),
            ("rtree", "CREATE VIRTUAL TABLE t USING rtree(id,minx,maxx,miny,maxy)", "virtual tables"),
            ("utf16", "PRAGMA encoding='UTF-16le'; CREATE TABLE t(n INT)", "UTF-16"),
            ("auto-vacuum-full", "PRAGMA auto_vacuum=FULL; VACUUM; CREATE TABLE t(n INT)", "auto_vacuum"),
            ("auto-vacuum-incremental", "PRAGMA auto_vacuum=INCREMENTAL; VACUUM; CREATE TABLE t(n INT)", "auto_vacuum"),
            ("reserved-name", "CREATE TABLE __romedb_catalog(name TEXT,metadata TEXT)", "reserved object"),
            ("unknown-view-function", "CREATE VIEW t AS SELECT made_up_function(1)", "cannot be queried"),
        ]
        for name, sql, reason in unsupported:
            directory = root / name
            directory.mkdir()
            source, destination = directory / "source.db", directory / "adopted.db"
            create(source, sql)
            original = source.read_bytes()
            reports = []
            for operation in ["check", "import"]:
                report = adoption(operation, source, destination if operation == "import" else None, succeeds=False)
                assert reason in json.dumps(report["unsupported"]), report
                assert not destination.exists() and source.read_bytes() == original
                assert list(directory.iterdir()) == [source], list(directory.iterdir())
                reports.append(report["unsupported"])
            cases.append({"name": name, "sourceUnchanged": True, "unpublished": True, "reasons": reports})

        protections = root / "protections"
        protections.mkdir()
        source, destination = protections / "source.db", protections / "adopted.db"
        create(source, "CREATE TABLE items(n INT)")
        original = source.read_bytes()
        invoke(["sqlite", "import", source, source], succeeds=False)
        for suffix in ["", "-wal", "-shm", "-journal"]:
            existing = destination.with_name(destination.name + suffix)
            existing.write_bytes(b"retain existing destination")
            invoke(["sqlite", "import", source, destination], succeeds=False)
            assert existing.read_bytes() == b"retain existing destination"
            existing.unlink()
        destination.hardlink_to(source)
        invoke(["sqlite", "import", source, destination], succeeds=False)
        assert source.read_bytes() == original
        destination.unlink()
        destination.symlink_to(protections / "nonexistent")
        invoke(["sqlite", "import", source, destination], succeeds=False)
        assert destination.is_symlink()
        destination.unlink()
        assert source.read_bytes() == original
        assert list(protections.iterdir()) == [source]
        cases.append({"name": "no-clobber-source-destination-sidecars-aliases", "sourceUnchanged": True,
                      "existingBytesRetained": True, "unpublished": True})

        corrupt = root / "corrupt"
        corrupt.mkdir()
        source, destination = corrupt / "source.db", corrupt / "adopted.db"
        source.write_bytes(bytes([255]) * 4096)
        original = source.read_bytes()
        for operation in ["check", "import"]:
            invoke(["sqlite", operation, source, *([destination] if operation == "import" else [])], succeeds=False)
            assert source.read_bytes() == original and not destination.exists()
            assert list(corrupt.iterdir()) == [source]
        cases.append({"name": "invalid-sqlite-file", "sourceUnchanged": True, "unpublished": True})

    report = {"cliSha256": sha(cli), "cliPath": str(cli), "pythonVersion": platform.python_version(),
              "sqliteProducerVersion": sqlite3.sqlite_version, "sqliteImporterVersions": sorted(importer_versions),
              "host": platform.platform(), "passed": len(cases), "cases": cases,
              "scope": "Exact supplied CLI adoption/check, source preservation and SQLite cross-engine readback; not all SQLite schemas or application queries"}
    evidence.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({"passed": len(cases), "evidence": str(evidence),
                      "sqliteProducerVersion": sqlite3.sqlite_version, "cliSha256": report["cliSha256"]}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("cli", type=Path, help="Exact installed bundle bin/romedb-cli")
    parser.add_argument("evidence", type=Path)
    arguments = parser.parse_args()
    verify(arguments.cli.resolve(), arguments.evidence.resolve())
