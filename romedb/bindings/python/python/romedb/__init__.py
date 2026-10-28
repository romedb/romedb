"""Typed embedded RomeDB. Connections serialize calls; transaction() also groups them."""

from __future__ import annotations

import contextlib
import dataclasses
import json
import math
import os
import struct
import threading
from collections.abc import Mapping, Sequence
from typing import Any, Iterator

from . import _native

__version__ = "0.2.11"
__all__ = [
    "Database",
    "Record",
    "Vector",
    "CancellationToken",
    "ResultLimits",
    "QueryResult",
    "ProfiledQuery",
    "Migration",
    "RomeDBError",
    "BatchError",
]


class RomeDBError(Exception):
    def __init__(self, message: str, code: str, transaction=None, migration=None):
        super().__init__(message)
        self.code = code
        self.transaction = transaction
        self.migration = migration


class BatchError(RomeDBError):
    def __init__(self, error: RomeDBError, reports: list):
        super().__init__(str(error), error.code, error.transaction, error.migration)
        self.reports = reports


@dataclasses.dataclass(frozen=True)
class Record:
    table: str
    key: str | int

    def __post_init__(self):
        if (
            not isinstance(self.table, str)
            or not self.table
            or self.table.startswith("__")
            or not self.table.isascii()
            or not (self.table[0].isalpha() or self.table[0] == "_")
            or not all(c.isalnum() or c == "_" for c in self.table)
        ):
            raise ValueError("record table must be an ordinary ASCII identifier")
        if type(self.key) not in (str, int):
            raise TypeError("record key must be a string or int64")
        _encode(self.key)
        if isinstance(self.key, str) and not self.key:
            raise ValueError("record key cannot be empty")
        object.__setattr__(self, "table", self.table.lower())


@dataclasses.dataclass(frozen=True)
class Vector:
    data: bytes

    def __post_init__(self):
        if not isinstance(self.data, bytes):
            raise TypeError("vector data must be bytes")

    @classmethod
    def from_components(cls, components: Sequence[float], encoding: str = "float32") -> Vector:
        values = list(components)
        if not 1 <= len(values) <= 65536 or any(type(v) not in (int, float) or not math.isfinite(v) for v in values):
            raise ValueError("vector needs 1..65536 finite numeric components")
        try:
            return cls(_native.vector(encoding, values))
        except RuntimeError as error:
            raise _native_error(error) from None


@dataclasses.dataclass(frozen=True)
class ResultLimits:
    max_rows: int
    max_payload_bytes: int

    def __post_init__(self):
        for value in (self.max_rows, self.max_payload_bytes):
            if type(value) is not int or not 0 <= value < 2**64:
                raise ValueError("result limits must be unsigned 64-bit integers")


@dataclasses.dataclass(frozen=True)
class QueryResult:
    columns: list[str]
    rows: list[list[Any]]
    affected: int
    transaction: dict[str, str]

    def first(self) -> list[Any] | None:
        return self.rows[0] if self.rows else None

    def exactly_one(self) -> list[Any]:
        if len(self.rows) != 1:
            raise RomeDBError(f"expected exactly one row, got {len(self.rows)}", "FDB_CARDINALITY", self.transaction)
        return self.rows[0]


@dataclasses.dataclass(frozen=True)
class ProfiledQuery:
    result: QueryResult
    metrics: dict[str, int]


@dataclasses.dataclass(frozen=True)
class Migration:
    version: int
    name: str
    sql: str

    def __post_init__(self):
        if type(self.version) is not int or not 0 < self.version < 2**63:
            raise ValueError("migration version must be a positive int64")
        if not isinstance(self.name, str) or not isinstance(self.sql, str):
            raise TypeError("migration name and SQL must be strings")


class CancellationToken:
    def __init__(self, timeout_ms: int | None = None):
        if timeout_ms is not None and (type(timeout_ms) is not int or not 0 <= timeout_ms < 2**32):
            raise ValueError("timeout_ms must be an unsigned 32-bit integer")
        self._native = _native.NativeCancellation(timeout_ms)

    def cancel(self) -> None:
        self._native.cancel()

    @property
    def cancelled(self) -> bool:
        return self._native.is_cancelled()


def _native_error(error):
    try:
        value = json.loads(str(error))
        return RomeDBError(value["message"], value["code"], migration=value.get("migration"))
    except (ValueError, KeyError, TypeError):
        return RomeDBError(str(error), "FDB_STORAGE")


def _integer(value):
    if not -(2**63) <= value < 2**63:
        raise OverflowError("integer exceeds int64")
    return str(value)


def _encode(value, depth=0, ancestors=None):  # noqa: C901 - one branch per wire value tag
    if depth > 64:
        raise ValueError("logical value depth exceeds 64")
    if value is None:
        return {"type": "Null"}
    if type(value) is bool:
        kind, data = "Boolean", value
    elif type(value) is int:
        kind, data = "Integer", _integer(value)
    elif type(value) is float:
        if not math.isfinite(value):
            raise ValueError("numbers must be finite")
        kind, data = "Number", struct.pack(">d", value).hex()
    elif type(value) is str:
        value.encode("utf-8")
        kind, data = "String", value
    elif isinstance(value, (bytes, bytearray, memoryview)):
        kind, data = "Binary", list(bytes(value))
    elif isinstance(value, Record):
        kind, data = "Record", {"table": value.table, "key": _encode(value.key)}
    elif isinstance(value, Vector):
        kind, data = "Vector", list(value.data)
    elif type(value) in (dict, list, tuple):
        return _encode_container(value, depth, ancestors)
    else:
        raise TypeError(f"unsupported RomeDB value: {type(value).__name__}")
    return {"type": kind, "value": data}


def _encode_container(value, depth, ancestors):
    ancestors = set() if ancestors is None else ancestors
    if id(value) in ancestors:
        raise ValueError("cyclic values are unsupported")
    ancestors.add(id(value))
    try:
        if isinstance(value, dict):
            if any(type(k) is not str for k in value):
                raise TypeError("object keys must be strings")
            for key in value:
                key.encode("utf-8")
            kind, data = "Object", {k: _encode(v, depth + 1, ancestors) for k, v in value.items()}
        else:
            kind, data = "Array", [_encode(v, depth + 1, ancestors) for v in value]
        return {"type": kind, "value": data}
    finally:
        ancestors.remove(id(value))


def _decode(value):
    kind, data = value["type"], value.get("value")
    if kind == "Null":
        return None
    if kind == "Integer":
        return int(data)
    if kind == "Number":
        return struct.unpack(">d", bytes.fromhex(data))[0]
    if kind in ("String", "Boolean"):
        return data
    if kind == "Binary":
        return bytes(data)
    if kind == "Vector":
        return Vector(bytes(data))
    if kind == "Record":
        return Record(data["table"], _decode(data["key"]))
    if kind == "Array":
        return [_decode(v) for v in data]
    if kind == "Object":
        return {k: _decode(v) for k, v in data.items()}
    raise RuntimeError(f"unsupported native value tag: {kind}")


def _params(parameters):
    if parameters is None:
        return {}
    if not isinstance(parameters, Mapping) or any(type(k) is not str for k in parameters):
        raise TypeError("parameters must map explicit parameter names to values")
    return {k: _encode(v) for k, v in parameters.items()}


def _query(value, transaction):
    return QueryResult(
        value["columns"], [[_decode(v) for v in row] for row in value["rows"]], value["affected"], transaction
    )


def _limits(value):
    if value is None:
        return None
    if not isinstance(value, ResultLimits):
        raise TypeError("limits must be ResultLimits")
    return dataclasses.asdict(value)


def _quote(name):
    if type(name) is not str or not name or "\0" in name:
        raise ValueError("name must be a nonempty string without NUL")
    return '"' + name.replace('"', '""') + '"'


class Database:
    def __init__(self, path: str | os.PathLike = ":memory:", *, write_buffer_limits: ResultLimits | None = None):
        path = os.fspath(path)
        if not isinstance(path, str) or "\0" in path:
            raise ValueError("path must be a string without NUL")
        limits = _limits(write_buffer_limits)
        self._lock, self._closed, self._savepoint = threading.RLock(), False, 0
        try:
            self._native = _native.NativeDatabase(path, json.dumps(limits) if limits is not None else None)
        except RuntimeError as error:
            raise _native_error(error) from None

    def _request(self, request, *, token=None, timeout_ms=None):
        if token is not None and timeout_ms is not None:
            raise ValueError("pass token or timeout_ms, not both")
        if token is None:
            token = CancellationToken(timeout_ms)
        if not isinstance(token, CancellationToken):
            raise TypeError("token must be CancellationToken")
        payload = json.dumps(request, ensure_ascii=False, allow_nan=False, separators=(",", ":"))
        with self._lock:
            if self._closed:
                raise RomeDBError("database is closed", "FDB_CLOSED")
            report = json.loads(self._native.call(payload, token._native))
        if report.get("version") != 1:
            raise RuntimeError("unsupported native protocol version")
        if "error" in report["execution"]:
            error = report["execution"]["error"]
            raise RomeDBError(error["message"], error["code"], report["transaction"], error.get("migration"))
        return report["execution"]["result"], report["transaction"]

    def execute(self, sql: str, parameters=None, *, token=None, timeout_ms=None) -> QueryResult:
        value, transaction = self._request(
            {"op": "execute", "sql": sql, "parameters": _params(parameters)}, token=token, timeout_ms=timeout_ms
        )
        return _query(value, transaction)

    def all(self, sql, parameters=None, **options):
        return self.execute(sql, parameters, **options).rows

    def first(self, sql, parameters=None, **options):
        return self.execute(sql, parameters, **options).first()

    def exactly_one(self, sql, parameters=None, **options):
        return self.execute(sql, parameters, **options).exactly_one()

    def select_with_limits(self, sql, parameters=None, *, limits: ResultLimits, **options):
        value, transaction = self._request(
            {"op": "execute", "sql": sql, "parameters": _params(parameters), "limits": _limits(limits)}, **options
        )
        return _query(value, transaction)

    def write_with_result_limits(self, sql, parameters=None, *, limits: ResultLimits, **options):
        value, transaction = self._request(
            {"op": "execute", "sql": sql, "parameters": _params(parameters), "limits": _limits(limits), "write": True},
            **options,
        )
        return _query(value, transaction)

    def profile_select(self, sql, parameters=None, *, limits=None, **options) -> ProfiledQuery:
        value, transaction = self._request(
            {"op": "profile", "sql": sql, "parameters": _params(parameters), "limits": _limits(limits)}, **options
        )
        return ProfiledQuery(_query(value["result"], transaction), value["metrics"])

    def execute_batch(self, sql: str, **options) -> list[dict]:
        reports, _ = self._request({"op": "batch", "sql": sql}, **options)
        for report in reports:
            execution = report.pop("execution")
            if "error" in execution:
                e = execution["error"]
                error = RomeDBError(e["message"], e["code"], report["transaction"], e.get("migration"))
                report["error"] = error
                raise BatchError(error, reports)
            report["result"] = _query(execution["result"], report["transaction"])
        return reports

    def migrate(self, migrations: Sequence[Migration], **options):
        plan = list(migrations)
        if not all(isinstance(m, Migration) for m in plan):
            raise TypeError("migrations must contain Migration objects")
        return self._request({"op": "migrate", "migrations": [dataclasses.asdict(m) for m in plan]}, **options)[0]

    def export_documents(self, table, format="json", **options):
        return self._request({"op": "export", "table": table, "format": format}, **options)[0]

    def import_documents(self, table, data, format="json", **options):
        return self._request({"op": "import", "table": table, "format": format, "input": data}, **options)[0]

    def check_collection_integrity(self, table, *, max_documents=100000, max_encoded_bytes=64 * 1024 * 1024, **options):
        for n in (max_documents, max_encoded_bytes):
            if type(n) is not int or not 0 <= n < 2**64:
                raise ValueError("integrity limits must be uint64")
        return self._request(
            {"op": "integrity", "table": table, "max_documents": max_documents, "max_encoded_bytes": max_encoded_bytes},
            **options,
        )[0]

    def collection(self, name: str) -> Collection:
        return Collection(self, name)

    @contextlib.contextmanager
    def transaction(self) -> Iterator[Database]:
        with self._lock:
            self._savepoint += 1
            name = f"romedb_python_{self._savepoint}"
            self.execute(f"SAVEPOINT {name}")
            try:
                yield self
                self.execute(f"RELEASE {name}")
            except BaseException as cause:
                if self.transaction_state == "active":
                    try:
                        self.execute(f"ROLLBACK TO {name}")
                        self.execute(f"RELEASE {name}")
                    except RomeDBError as cleanup:
                        raise RomeDBError(
                            f"rollback failed after {cause}: {cleanup}", "FDB_ROLLBACK", cleanup.transaction
                        ) from cause
                raise

    @property
    def transaction_state(self) -> str:
        return self._request({"op": "state"})[0]

    def interrupt(self) -> bool:
        return self._native.interrupt()

    def close(self) -> None:
        with self._lock:
            if not self._closed:
                self._native.close()
                self._closed = True

    def __enter__(self):
        if self._closed:
            raise RomeDBError("database is closed", "FDB_CLOSED")
        return self

    def __exit__(self, *args):
        self.close()


class Collection:
    def __init__(self, database: Database, name: str):
        self._database, self._name = database, Record(name, "validate").table
        self._sql = _quote(self._name)

    def get(self, key: str | int, **options):
        record = Record(self._name, key)
        row = self._database.first(f"SELECT doc::row(t) FROM {self._sql} t WHERE t.id=$id", {"$id": record}, **options)
        return None if row is None else row[0]

    def insert(self, document: dict, **options):
        return self._database.exactly_one(
            f"INSERT INTO {self._sql} DOCUMENT $document RETURNING *", {"$document": document}, **options
        )[0]

    def patch(self, key: str | int, patch: dict, **options):
        if type(patch) is not dict:
            raise TypeError("patch must be an object")
        params = {"$id": Record(self._name, key)}
        fields = []
        for i, (key, value) in enumerate(patch.items()):
            fields.append(f"{_quote(key)}:$field{i}")
            params[f"$field{i}"] = value
        result = self._database.first(
            f"UPDATE {self._sql} {{{','.join(fields)}}} WHERE id=$id RETURNING *", params, **options
        )
        return None if result is None else result[0]

    def delete(self, key: str | int, **options):
        row = self._database.first(
            f"DELETE FROM {self._sql} WHERE id=$id RETURNING *", {"$id": Record(self._name, key)}, **options
        )
        return None if row is None else row[0]
