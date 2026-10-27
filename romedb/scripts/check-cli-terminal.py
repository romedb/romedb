#!/usr/bin/env python3
"""Exercise the CLI through a real Unix controlling terminal, keeping JSON separate."""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import subprocess
import sys
import tempfile
import termios
import time


class Terminal:
    def __init__(self, binary, *args):
        master, slave = pty.openpty()
        self.master = master
        self.text = b""
        self.json_buffer = b""
        self.rows = []

        def session():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        self.child = subprocess.Popen(
            [binary, *args, ":memory:"], stdin=slave, stdout=subprocess.PIPE,
            stderr=slave, preexec_fn=session, env={**os.environ, "TERM": "xterm"},
        )
        os.close(slave)
        try:
            self.wait(lambda: b"romedb> " in self.text)
        except BaseException:
            self.cleanup()
            raise

    def wait(self, predicate):
        deadline = time.monotonic() + 10
        while not predicate():
            if time.monotonic() > deadline:
                raise AssertionError(("terminal timeout", self.text, self.rows))
            ready, _, _ = select.select([self.master, self.child.stdout], [], [], 0.1)
            for stream in ready:
                try:
                    data = os.read(stream if isinstance(stream, int) else stream.fileno(), 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    data = b""
                if stream == self.master:
                    self.text += data
                    if b"\x1b[6n" in data:
                        os.write(self.master, b"\x1b[1;1R")
                else:
                    self.json_buffer += data
                    while b"\n" in self.json_buffer:
                        line, self.json_buffer = self.json_buffer.split(b"\n", 1)
                        self.rows.append(json.loads(line))
            if self.child.poll() is not None and not predicate():
                raise AssertionError(("unexpected CLI exit", self.child.returncode, self.text, self.rows))

    def send(self, data, rows=None, prompt=None):
        self.text = b""
        os.write(self.master, data)
        self.wait(lambda: (rows is None or len(self.rows) >= rows)
                  and (prompt is None or prompt in self.text))

    def close(self, expected_status=0):
        if self.child.poll() is None:
            os.write(self.master, b".quit\n")
        status = self.child.wait(timeout=10)
        assert status == expected_status, (status, self.text, self.rows)
        self.child.stdout.close()
        os.close(self.master)

    def cleanup(self):
        if self.child.poll() is None:
            os.killpg(self.child.pid, signal.SIGKILL)
            self.child.wait(timeout=10)
        self.child.stdout.close()
        try:
            os.close(self.master)
        except OSError:
            pass


def main():
    binary = str(Path(sys.argv[1]).resolve())
    with tempfile.TemporaryDirectory(prefix="romedb-terminal-") as directory:
        history = Path(directory) / "history"
        terminal = Terminal(binary, "--history", str(history))
        try:
            terminal.send(b"SELECT 1;\n", rows=1, prompt=b"romedb> ")
            terminal.send(b"\x1b[A\n", rows=2, prompt=b"romedb> ")
            assert terminal.rows[1]["rows"][0][0]["value"] == 1
            terminal.send(b"SELECT 2x;\x1b[D\x7f\n", rows=3, prompt=b"romedb> ")
            assert terminal.rows[2]["rows"][0][0]["value"] == 2
            terminal.send(b"SELECT\n", prompt=b"...> ")
            terminal.send(b"3;\n", rows=4, prompt=b"romedb> ")
            terminal.send(b"\x1b[A\n", rows=5, prompt=b"romedb> ")
            assert terminal.rows[4]["rows"][0][0]["value"] == 3
            terminal.send(b"BEGIN;\n", rows=6, prompt=b"romedb(tx)> ")
            terminal.send(b"SELECT 'unfinished\n", prompt=b"...> ")
            terminal.send(b"\x03", prompt=b"romedb(tx)> ")
            terminal.send(b"SELECT 4;\n", rows=7, prompt=b"romedb(tx)> ")
            assert terminal.rows[6]["transaction"]["before"] == "active"
            assert terminal.rows[6]["rows"][0][0]["value"] == 4
            terminal.send(b"ROLLBACK;\n", rows=8, prompt=b"romedb> ")
            terminal.send(b"SELECT 7;\n", rows=9, prompt=b"romedb> ")
            terminal.send(b" SELECT 99;\n", rows=10, prompt=b"romedb> ")
            terminal.close()
            contents = history.read_text()
            assert "unfinished" not in contents and "99" not in contents
            assert history.stat().st_mode & 0o077 == 0
        finally:
            terminal.cleanup()
        terminal = Terminal(binary, "--history", str(history))
        try:
            terminal.send(b"\x1b[A\n", rows=1, prompt=b"romedb> ")
            assert terminal.rows[0]["rows"][0][0]["value"] == 7
            terminal.close()
        finally:
            terminal.cleanup()
        terminal = Terminal(binary, "--max-input-bytes", "8")
        try:
            terminal.send(b"SELECT 1234;\n", rows=1)
            assert terminal.rows[0]["error"]["code"] == "FDB_LIMIT"
            assert terminal.child.wait(timeout=10) == 1
        finally:
            terminal.cleanup()
        terminal = Terminal(binary)
        try:
            terminal.send(b"\x04")
            assert terminal.child.wait(timeout=10) == 0
        finally:
            terminal.cleanup()
        terminal = Terminal(binary)
        try:
            values = ",".join(f"({n})" for n in range(1, 81))
            terminal.send(("CREATE TABLE numbers(x INTEGER); INSERT INTO numbers VALUES " + values + "; CREATE TABLE sink(x INTEGER);\n").encode(), rows=3, prompt=b"romedb> ")
            source = " FROM numbers a CROSS JOIN numbers b CROSS JOIN numbers c CROSS JOIN numbers d"
            for statement in ["SELECT sum(a.x+b.x+c.x+d.x)" + source,
                              "INSERT INTO sink SELECT a.x" + source]:
                outer = statement.startswith("SELECT")
                if outer:
                    terminal.send(b"BEGIN; INSERT INTO sink VALUES (123);\n", rows=len(terminal.rows) + 2, prompt=b"romedb(tx)> ")
                before = len(terminal.rows)
                label = b"romedb(tx)> " if outer else b"romedb> "
                # A flushed marker proves readline has returned and the batch
                # has started. Repeat Ctrl-C through possible prepare windows.
                terminal.send(("SELECT 42; " + statement + "; SELECT 99;\n").encode(), rows=before + 1)
                deadline = time.monotonic() + 10
                while len(terminal.rows) < before + 2:
                    assert time.monotonic() < deadline, "query did not cancel"
                    os.write(terminal.master, b"\x03")
                    poll = time.monotonic() + 0.1
                    terminal.wait(lambda: len(terminal.rows) >= before + 2 or time.monotonic() >= poll)
                terminal.wait(lambda: label in terminal.text)
                assert len(terminal.rows) == before + 2, terminal.rows
                assert terminal.rows[-1]["error"]["code"] == "FDB_CANCELLED", terminal.rows[-1]
                assert terminal.rows[-1]["transaction"]["after"] == ("active" if outer else "autocommit")
                terminal.send(b"SELECT count(*) FROM sink;\n", rows=before + 3, prompt=label)
                assert terminal.rows[-1]["rows"][0][0]["value"] == (1 if outer else 0)
                if outer:
                    terminal.send(b"ROLLBACK;\n", rows=before + 4, prompt=b"romedb> ")
            terminal.close(expected_status=1)
        finally:
            terminal.cleanup()
        terminal = Terminal(binary)
        try:
            values = ",".join(f"({n})" for n in range(1, 81))
            terminal.send(("CREATE TABLE numbers(x INTEGER); INSERT INTO numbers VALUES " + values + "; CREATE TABLE sink(x INTEGER); CREATE TABLE docs;\n").encode(), rows=4, prompt=b"romedb> ")
            source = " FROM numbers a CROSS JOIN numbers b CROSS JOIN numbers c CROSS JOIN numbers d"
            commands = [
                ".select-limit 1 9 SELECT count(*) AS n" + source,
                ".profile-limit 1 9 SELECT count(*) AS n" + source,
                ".write-limit 1 9 INSERT INTO sink SELECT count(*)" + source + " RETURNING x",
                ".write-limit 1 9 INSERT INTO docs(n) SELECT count(*)" + source + " RETURNING n",
            ]
            for command in commands:
                terminal.send(b"BEGIN; INSERT INTO sink VALUES(123); INSERT INTO docs {id:docs:prior,n:123};\n", rows=len(terminal.rows)+3, prompt=b"romedb(tx)> ")
                before = len(terminal.rows)
                assert not termios.tcgetattr(terminal.master)[3] & termios.ICANON
                terminal.send((command + ";\n").encode())
                # Rustyline restores canonical mode after accepting the command.
                # This distinguishes engine/planning cancellation from clearing
                # the still-edited input at the prompt.
                terminal.wait(lambda: len(terminal.rows) > before or bool(termios.tcgetattr(terminal.master)[3] & termios.ICANON))
                deadline = time.monotonic() + 10
                while len(terminal.rows) == before:
                    assert time.monotonic() < deadline, "limited command did not cancel"
                    os.write(terminal.master, b"\x03")
                    poll = time.monotonic() + 0.1
                    terminal.wait(lambda: len(terminal.rows) > before or time.monotonic() >= poll)
                terminal.wait(lambda: b"romedb(tx)> " in terminal.text)
                report = terminal.rows[-1]
                assert report["error"]["code"] == "FDB_CANCELLED", report
                assert report["transaction"] == {"before": "active", "after": "active"}, report
                assert "rows" not in report and "profile" not in report
                terminal.send(b".select-limit 1 9 SELECT x FROM sink;\n", rows=before+2, prompt=b"romedb(tx)> ")
                assert terminal.rows[-1]["rows"][0][0]["value"] == 123
                terminal.send(b".profile-limit 1 9 SELECT n FROM docs;\n", rows=before+3, prompt=b"romedb(tx)> ")
                assert terminal.rows[-1]["rows"][0][0]["value"] == 123
                assert "profile" in terminal.rows[-1]
                terminal.send(b".write-limit 1 9 INSERT INTO sink VALUES(124) RETURNING x;\n", rows=before+4, prompt=b"romedb(tx)> ")
                assert terminal.rows[-1]["rows"][0][0]["value"] == 124
                terminal.send(b"ROLLBACK;\n", rows=before+5, prompt=b"romedb> ")
            terminal.close(expected_status=1)
        finally:
            terminal.cleanup()
    print("CLI terminal editing, history, Ctrl-C cancellation and JSON isolation passed")


if __name__ == "__main__":
    main()
