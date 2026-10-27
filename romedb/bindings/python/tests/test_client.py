import math
import pathlib
import tempfile
import threading
import unittest

from romedb import BatchError, CancellationToken, Database, RomeDBError, Migration, Record, ResultLimits, Vector


class ClientTests(unittest.TestCase):
    def test_v22_native_contract_fixture(self):
        import json
        import struct

        fixture = pathlib.Path(__file__).resolve().parents[2] / "fixtures/native-client.json"
        steps = [step for step in json.loads(fixture.read_text()) if step.get("release") in ("2.2.0", "2.5.1")]
        self.assertGreaterEqual(len(steps), 50)

        def value(tagged):
            kind = tagged["type"]
            raw = tagged.get("value")
            if kind == "Null":
                return None
            if kind == "Integer":
                return int(raw)
            if kind in ("String", "Boolean"):
                return raw
            if kind == "Number":
                return struct.unpack(">d", bytes.fromhex(raw))[0]
            if kind == "Binary":
                return bytes(raw)
            if kind == "Record":
                return Record(raw["table"], value(raw["key"]))
            if kind == "Array":
                return [value(item) for item in raw]
            if kind == "Object":
                return {key: value(item) for key, item in raw.items()}
            self.fail(f"Unhandled fixture value {kind}")

        with tempfile.TemporaryDirectory() as directory:
            file = pathlib.Path(directory) / "v22.db"
            db = Database(file)
            try:
                for index, step in enumerate(steps):
                    with self.subTest(step=index):
                        if step.get("reopen"):
                            db.close()
                            db = Database(file)
                            continue
                        request = step["request"]
                        if request["op"] == "integrity":
                            self.assertEqual(db.check_collection_integrity(request["table"])["documents"], 1)
                            continue
                        self.assertEqual(request["op"], "execute")
                        params = {name: value(item) for name, item in request["parameters"].items()}
                        if "error" in step:
                            with self.assertRaises(RomeDBError) as failure:
                                db.execute(request["sql"], params)
                            self.assertEqual(failure.exception.code, step["error"])
                        else:
                            result = db.execute(request["sql"], params)
                            if "rows" in step:
                                self.assertEqual(result.rows, [[value(item) for item in row] for row in step["rows"]])
            finally:
                db.close()

    def test_typed_roundtrip_duplicate_columns_and_persistence(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "typed.db"
            value = {
                "id": Record("docs", "α"),
                "integer": 2**63 - 1,
                "minimum": -(2**63),
                "float": 1.25,
                "zero": -0.0,
                "tiny": 5e-324,
                "truth": True,
                "nothing": None,
                "binary": b"\x00\xffFDB\x01",
                "text": "\"'{}\\\n",
                "nested": [{"x": False}, [1, 2.0]],
                "vector": Vector.from_components([1, 2, 3]),
            }
            with Database(path) as db:
                actual = db.collection("docs").insert(value)
                self.assertEqual(actual, value)
                self.assertLess(math.copysign(1, actual["zero"]), 0)
                row = db.collection("docs").get("α")
                self.assertEqual(row, value)
                projected = db.execute("SELECT integer AS x,truth AS x,nested,binary,vector FROM docs")
                self.assertEqual(projected.columns, ["x", "x", "nested", "binary", "vector"])
                self.assertEqual(projected.rows[0][:2], [2**63 - 1, True])
                self.assertEqual(db.exactly_one("SELECT $n,$b", {"$n": 2**63 - 1, "$b": b"x"}), [2**63 - 1, b"x"])
                db.collection("docs").patch("α", {"nested": [1, 2], 'quote"key': "value"})
                self.assertEqual(db.collection("docs").get("α")['quote"key'], "value")
            with Database(path) as db:
                old = db.collection("docs").delete("α")
                self.assertEqual(old["id"], Record("docs", "α"))
                self.assertIsNone(db.collection("docs").get("α"))
                self.assertIsNone(db.collection("docs").delete("α"))
                self.assertEqual(db.check_collection_integrity("docs")["documents"], 0)

    def test_transaction_context_and_limits_preserve_indexes(self):
        with Database() as db:
            db.execute("CREATE TABLE docs")
            db.execute("CREATE UNIQUE INDEX docs_n ON docs(n)")
            with db.transaction():
                db.collection("docs").insert({"n": 1})
                with self.assertRaises(RomeDBError):
                    with db.transaction():
                        db.collection("docs").insert({"n": 2})
                        db.collection("docs").insert({"n": 1})
                self.assertEqual(db.all("SELECT n FROM docs"), [[1]])
                with self.assertRaises(RomeDBError) as failure:
                    db.write_with_result_limits("UPDATE docs SET n=9 RETURNING n", limits=ResultLimits(0, 100))
                self.assertEqual(failure.exception.code, "FDB_LIMIT")
                self.assertEqual(failure.exception.transaction, {"before": "active", "after": "active"})
                self.assertEqual(db.all("SELECT n FROM docs"), [[1]])
            self.assertEqual(db.transaction_state, "autocommit")
            self.assertEqual(db.check_collection_integrity("docs")["index_entries"], 1)
            with self.assertRaises(RomeDBError):
                db.select_with_limits("SELECT n FROM docs", limits=ResultLimits(0, 100))
            report = db.profile_select("SELECT n FROM docs WHERE n=1")
            self.assertEqual(report.result.rows, [[1]])
            self.assertGreater(report.metrics["vm_steps"], 0)
            with self.assertRaises(ValueError):
                with db.transaction():
                    db.collection("docs").insert({"n": 3})
                    raise ValueError("caller failure")
            self.assertEqual(db.all("SELECT n FROM docs"), [[1]])

    def test_v2_search_and_javascript(self):
        with Database() as db:
            db.execute(
                "INSERT INTO places {id:places:a,title:'coffee database',point:geo::point(100,13),v:vector32('[1,0]')}"
            )
            db.execute(
                "INSERT INTO places {id:places:b,title:'tea database',point:geo::point(101,14),v:vector32('[0,1]')}"
            )
            db.execute("CREATE SEARCH INDEX vectors ON places(v) USING VECTOR WITH (dimensions=2,metric='cosine')")
            db.execute("CREATE SEARCH INDEX texts ON places(title) USING FULLTEXT")
            hit = db.exactly_one("SELECT id,distance FROM search::vector('vectors',vector32('[1,0]'),1)")
            self.assertEqual(hit, [Record("places", "a"), 0.0])
            self.assertEqual(
                db.exactly_one("SELECT id FROM search::text('texts','coffee',10)"), [Record("places", "a")]
            )
            self.assertEqual(db.all("PRAGMA integrity_check"), [["ok"]])
            self.assertEqual(db.all("PRAGMA quick_check"), [["ok"]])
            db.execute(
                "CREATE FUNCTION app::lower(v string) RETURNS string LANGUAGE JAVASCRIPT AS 'return v.toLowerCase();'"
            )
            self.assertEqual(db.exactly_one("SELECT app::lower('ABC')"), ["abc"])
            self.assertEqual(db.check_collection_integrity("places")["documents"], 2)
            cell = db.exactly_one("SELECT geo::cell(geo::point(100,13),7)")[0]
            self.assertIsInstance(cell, str)
            db.execute("DROP INDEX texts")
            self.assertEqual(db.all("PRAGMA integrity_check"), [["ok"]])

    def test_javascript_errors_preserve_savepoints_and_prior_work(self):
        with Database() as db:
            db.execute("CREATE TABLE docs")
            db.execute("CREATE UNIQUE INDEX docs_n ON docs(n)")
            db.execute("CREATE FUNCTION app::fail() RETURNS integer LANGUAGE JAVASCRIPT AS 'throw Error(\"fixture\");'")
            db.execute("CREATE FUNCTION app::late(n integer) RETURNS integer LANGUAGE JAVASCRIPT AS 'if(n===2n)throw Error(\"late\");return n+10n;'")
            db.execute("CREATE FUNCTION app::runaway() RETURNS any LANGUAGE JAVASCRIPT AS 'while(true){}'")
            with db.transaction():
                db.execute("INSERT INTO docs {n:1}")
                with self.assertRaises(RomeDBError) as failure:
                    with db.transaction():
                        db.execute("INSERT INTO docs {n:2}")
                        db.execute("SELECT app::fail()")
                self.assertEqual(failure.exception.code, "FDB_VALIDATION")
                self.assertEqual(failure.exception.transaction, {"before": "active", "after": "active"})
                self.assertEqual(db.transaction_state, "active")
                self.assertEqual(db.all("SELECT n FROM docs"), [[1]])
                db.execute("INSERT INTO docs {n:2}")
                with self.assertRaises(RomeDBError) as failure:
                    db.execute("UPDATE docs SET n=app::late(n)")
                self.assertEqual(failure.exception.transaction, {"before": "active", "after": "active"})
                self.assertEqual(db.all("SELECT n FROM docs ORDER BY n"), [[1], [2]])
                self.assertEqual(db.check_collection_integrity("docs")["index_entries"], 2)
                with self.assertRaises(RomeDBError) as failure:
                    db.execute("SELECT app::runaway()", timeout_ms=5)
                self.assertEqual(failure.exception.code, "FDB_CANCELLED")
                self.assertEqual(failure.exception.transaction, {"before": "active", "after": "active"})
                self.assertEqual(db.all("SELECT n FROM docs ORDER BY n"), [[1], [2]])
            self.assertEqual(db.transaction_state, "autocommit")
            self.assertEqual(db.all("SELECT n FROM docs ORDER BY n"), [[1], [2]])

    def test_migrations_transfer_and_batch_reports(self):
        with Database() as db:
            plan = [Migration(1, "create", "CREATE TABLE docs; CREATE UNIQUE INDEX docs_n ON docs(n);")]
            self.assertEqual(db.migrate(plan)["applied"], [1])
            self.assertEqual(db.migrate(plan)["already_applied"], 1)
            db.collection("docs").insert({"id": Record("docs", "one"), "n": 1, "b": b"\xff"})
            for format in ["json", "ndjson"]:
                bundle = db.export_documents("docs", format)
                with Database() as copy:
                    copy.execute("CREATE TABLE docs")
                    self.assertEqual(copy.import_documents("docs", bundle, format), 1)
                    self.assertEqual(copy.collection("docs").get("one"), db.collection("docs").get("one"))
            with self.assertRaises(BatchError) as failure:
                db.execute_batch("INSERT INTO docs {n:2}; INSERT INTO docs {n:1}; INSERT INTO docs {n:3};")
            reports = failure.exception.reports
            self.assertEqual(len(reports), 2)
            self.assertEqual(reports[0]["result"].affected, 1)
            self.assertEqual(reports[1]["error"].code, "FDB_CONSTRAINT")
            self.assertEqual(db.all("SELECT n FROM docs ORDER BY n"), [[1], [2]])
            with self.assertRaises(RomeDBError) as failure:
                db.migrate(plan + [Migration(2, "bad", "INSERT INTO docs {n:3}; INSERT INTO docs {n:1};")])
            self.assertEqual(failure.exception.code, "FDB_MIGRATION")
            self.assertEqual(failure.exception.migration["version"], 2)
            self.assertEqual(db.all("SELECT n FROM docs ORDER BY n"), [[1], [2]])

    def test_cancellation_releases_gil_and_keeps_prior_work(self):
        with Database() as db:
            db.execute("CREATE TABLE nums(n INTEGER)")
            db.execute("CREATE TABLE sink(n INTEGER)")
            db.execute("INSERT INTO nums VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9)")
            db.execute("CREATE TABLE docs")
            heavy = "SELECT count(*) FROM nums a,nums b,nums c,nums d,nums e,nums f,nums g,nums h,nums i"
            with db.transaction():
                db.collection("docs").insert({"n": 1})
                token = CancellationToken()
                timer = threading.Timer(0.03, token.cancel)
                timer.start()
                try:
                    with self.assertRaises(RomeDBError) as failure:
                        db.execute(heavy, token=token)
                finally:
                    timer.join()
                self.assertTrue(token.cancelled)
                self.assertEqual(failure.exception.code, "FDB_CANCELLED")
                self.assertEqual(db.all("SELECT n FROM docs"), [[1]])
                write_token = CancellationToken()
                write_timer = threading.Timer(0.03, write_token.cancel)
                write_timer.start()
                try:
                    with self.assertRaises(RomeDBError) as failure:
                        db.execute(
                            "INSERT INTO sink SELECT a.n FROM nums a,nums b,nums c,nums d,"
                            "nums e,nums f,nums g,nums h,nums i",
                            token=write_token,
                        )
                finally:
                    write_timer.join()
                self.assertEqual(failure.exception.code, "FDB_CANCELLED")
                self.assertEqual(db.all("SELECT count(*) FROM sink"), [[0]])
                self.assertEqual(db.all("SELECT n FROM docs"), [[1]])
                with self.assertRaises(RomeDBError) as failure:
                    db.execute(heavy, timeout_ms=1)
                self.assertEqual(failure.exception.code, "FDB_CANCELLED")
            self.assertEqual(db.all("SELECT n FROM docs"), [[1]])
            token = CancellationToken()
            token.cancel()
            with self.assertRaises(RomeDBError):
                db.execute("INSERT INTO docs {n:2}", token=token)
            self.assertEqual(db.all("SELECT n FROM docs"), [[1]])

    def test_invalid_values_and_close_rollback(self):
        cyclic = []
        cyclic.append(cyclic)
        with Database() as db:
            for value in [2**63, float("inf"), float("nan"), cyclic, {1: "bad"}, object(), "\ud800"]:
                with self.assertRaises((ValueError, TypeError, OverflowError)):
                    db.execute("SELECT $v", {"$v": value})
            with self.assertRaises(RomeDBError):
                db.execute("SELECT $v", {"$v": Vector(b"invalid")})
            with self.assertRaises(RomeDBError) as failure:
                db.exactly_one("SELECT 1 WHERE 0")
            self.assertEqual(failure.exception.code, "FDB_CARDINALITY")
            self.assertIsNone(db.first("SELECT 1 WHERE 0"))
        db.close()
        with self.assertRaises(RomeDBError) as failure:
            db.execute("SELECT 1")
        self.assertEqual(failure.exception.code, "FDB_CLOSED")
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "close.db"
            db = Database(path)
            db.execute("CREATE TABLE docs")
            db.execute("BEGIN")
            db.collection("docs").insert({"n": 1})
            db.close()
            with Database(path) as check:
                self.assertEqual(check.all("SELECT * FROM docs"), [])

    def test_queued_timeout_cannot_join_another_threads_transaction(self):
        with Database() as db:
            db.execute("CREATE TABLE docs")
            ready = threading.Event()
            errors = []

            def queued_write():
                token = CancellationToken(timeout_ms=1)
                ready.set()
                try:
                    db.collection("docs").insert({"n": 2}, token=token)
                except RomeDBError as error:
                    errors.append(error)

            with db.transaction():
                db.collection("docs").insert({"n": 1})
                worker = threading.Thread(target=queued_write)
                worker.start()
                self.assertTrue(ready.wait(5))
                threading.Event().wait(0.03)
                self.assertEqual(db.all("SELECT n FROM docs"), [[1]])
            worker.join(5)
            self.assertFalse(worker.is_alive())
            self.assertEqual([error.code for error in errors], ["FDB_CANCELLED"])
            self.assertEqual(db.all("SELECT n FROM docs"), [[1]])


if __name__ == "__main__":
    unittest.main()
