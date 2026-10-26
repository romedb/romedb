#!/usr/bin/env python3
"""Exercise native ownership, malformed input, concurrent close and interruption."""
import ctypes
import json
import sys
import threading
import time
import unittest

lib = ctypes.CDLL(sys.argv.pop(1))
lib.fdb_abi_version.restype = ctypes.c_uint32
for name, args in {
    "fdb_open": [ctypes.c_char_p],
    "fdb_call": [ctypes.c_uint64, ctypes.c_char_p, ctypes.c_int64],
    "fdb_close": [ctypes.c_uint64],
    "fdb_interrupt": [ctypes.c_uint64],
}.items():
    f = getattr(lib, name)
    f.argtypes = args
    f.restype = ctypes.c_void_p
lib.fdb_free.argtypes = [ctypes.c_void_p]
lib.fdb_free.restype = None


def receive(pointer):
    assert pointer
    try:
        return json.loads(ctypes.string_at(pointer))
    finally:
        lib.fdb_free(pointer)


def call(handle, sql, timeout=-1):
    request = json.dumps({"op": "execute", "sql": sql, "parameters": {}}).encode()
    return receive(lib.fdb_call(handle, request, timeout))


class ABI(unittest.TestCase):
    def setUp(self):
        self.handle = int(receive(lib.fdb_open(b":memory:"))["execution"]["result"])

    def tearDown(self):
        receive(lib.fdb_close(self.handle))

    def test_validation_and_stale_handles(self):
        self.assertEqual(lib.fdb_abi_version(), 1)
        lib.fdb_free(None)
        for text in [None, b"\xff"]:
            self.assertEqual(receive(lib.fdb_open(text))["execution"]["error"]["code"], "FDB_VALIDATION")
        for text in [None, b"\xff", b"{", b'{"op":"state","op":"state"}', b'{"op":"execute","sql":"SELECT 1","parameters":{},"extra":1}']:
            self.assertIn("error", receive(lib.fdb_call(self.handle, text, -1))["execution"])
        self.assertIn("error", call(self.handle, "SELECT 1", -2)["execution"])
        receive(lib.fdb_close(self.handle))
        receive(lib.fdb_close(self.handle))
        self.assertIn("error", call(self.handle, "SELECT 1")["execution"])
        newer = int(receive(lib.fdb_open(b":memory:"))["execution"]["result"])
        self.assertGreater(newer, self.handle)
        receive(lib.fdb_close(newer))

    def test_concurrent_connections(self):
        failures = []
        handles = []

        def worker():
            try:
                handle = int(receive(lib.fdb_open(b":memory:"))["execution"]["result"])
                handles.append(handle)
                try:
                    self.assertIn("result", call(handle, "SELECT 42")["execution"])
                finally:
                    receive(lib.fdb_close(handle))
            except BaseException as error:
                failures.append(error)

        threads = [threading.Thread(target=worker) for _ in range(12)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(10)
            self.assertFalse(thread.is_alive())
        self.assertEqual(failures, [])
        self.assertEqual(len(set(handles)), 12)

    def check_cancelled_plain_write(self, start, finish):
        def execute(sql):
            response = call(self.handle, sql)
            self.assertIn("result", response["execution"], f"{sql}: {response}")
            return response

        def integers(sql):
            rows = execute(sql)["execution"]["result"]["rows"]
            return [[int(cell["value"]) for cell in row] for row in rows]

        for sql in [
            "CREATE TABLE nums(n INTEGER)",
            "CREATE TABLE sink(n INTEGER)",
            "CREATE TABLE prior(n INTEGER)",
            "INSERT INTO nums VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9)",
            start,
            "INSERT INTO prior VALUES(99)",
        ]:
            execute(sql)
        self.assertEqual(integers("SELECT last_insert_rowid()"), [[1]])
        # A slow host can deliver the interrupt before the first insert. Each such
        # attempt must still cancel cleanly; retry with a longer delay until one
        # interrupts a writer that has inserted rows.
        for delay in [0.05, 0.1, 0.2, 0.4, 0.8, 1.6]:
            self.interrupt_plain_write(delay)
            self.assertEqual(integers("SELECT n FROM sink"), [])
            self.assertEqual(integers("SELECT n FROM prior"), [[99]])
            # Savepoint rollback retains this value, proving native inserts happened.
            if integers("SELECT last_insert_rowid()")[0][0] > 1:
                break
        else:
            self.fail("Cancellation must interrupt an in-flight native writer")
        execute("INSERT INTO sink VALUES(7)")
        self.assertEqual(execute(finish)["transaction"], {"before": "active", "after": "autocommit"})
        self.assertEqual(integers("SELECT n FROM prior"), [[99]])
        self.assertEqual(integers("SELECT n FROM sink"), [[7]])

    def interrupt_plain_write(self, delay):
        results = []
        failures = []
        started = threading.Event()

        def write():
            try:
                started.set()
                begin = time.monotonic()
                # Bound a missed interrupt; completing this workload is not a pass.
                response = call(self.handle,
                    "INSERT INTO sink SELECT a.n FROM nums a,nums b,nums c,nums d,nums e,nums f,nums g,nums h,nums i", 3000)
                results.append((response, time.monotonic() - begin))
            except BaseException as error:
                failures.append(error)

        active = threading.Thread(target=write)
        active.start()
        try:
            self.assertTrue(started.wait(1))
            time.sleep(delay)
            self.assertTrue(active.is_alive(), "Write completed before interruption")
            # The engine ignores an interrupt that arrives before its statement
            # starts, so repeat it until the writer returns.
            while active.is_alive():
                self.assertEqual(receive(lib.fdb_interrupt(self.handle))["execution"], {"result": True})
                active.join(0.01)
        finally:
            active.join(5)
        self.assertFalse(active.is_alive())
        self.assertEqual(failures, [])
        response, elapsed = results[0]
        self.assertEqual(response["execution"]["error"]["code"], "FDB_CANCELLED")
        self.assertLess(elapsed, 3, "The write reached its deadline instead of being interrupted")
        self.assertEqual(response["transaction"], {"before": "active", "after": "active"})

    def test_cancelled_plain_write_preserves_commit(self):
        self.check_cancelled_plain_write("BEGIN", "COMMIT")

    def test_cancelled_plain_write_preserves_root_release(self):
        self.check_cancelled_plain_write("SAVEPOINT caller", "RELEASE caller")

    def test_close_remains_interruptible(self):
        # A long query with its own deadline prevents the regression from hanging
        # the test process if close makes its handle unreachable too early.
        self.assertIn("result", call(self.handle, "CREATE TABLE numbers(x INTEGER)")["execution"])
        self.assertIn("result", call(self.handle, "INSERT INTO numbers VALUES " + ",".join(f"({n})" for n in range(1000)))["execution"])
        results = []
        started = threading.Event()

        def query():
            started.set()
            results.append(call(self.handle,
                "SELECT sum(a.x+b.x+c.x) FROM numbers a,numbers b,numbers c", 3000))

        active = threading.Thread(target=query)
        active.start()
        self.assertTrue(started.wait(1))
        time.sleep(0.1)
        self.assertTrue(active.is_alive())
        closed = []
        closer = threading.Thread(target=lambda: closed.append(receive(lib.fdb_close(self.handle))))
        closer.start()
        time.sleep(0.1)
        interrupted = receive(lib.fdb_interrupt(self.handle))
        active.join(5)
        closer.join(5)
        self.assertFalse(active.is_alive())
        self.assertFalse(closer.is_alive())
        self.assertEqual(interrupted["execution"], {"result": True})
        self.assertEqual(results[0]["execution"]["error"]["code"], "FDB_CANCELLED")
        self.assertEqual(closed[0]["execution"], {"result": None})


if __name__ == "__main__":
    unittest.main()
