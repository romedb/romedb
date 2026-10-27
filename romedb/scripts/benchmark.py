#!/usr/bin/env python3
"""Reproducible CLI round-trip benchmark; not a release performance certificate."""
import argparse
import hashlib
import heapq
import json
import math
import os
from pathlib import Path
import platform
import random
import select
import statistics
import struct
import subprocess
import tempfile
import time


def vector_for(key, dimensions, fixture):
    if key == 0:
        return [1.0] + [0.0] * (dimensions - 1)
    if fixture == "cyclic":
        return [1.0] + [0.1 + ((key * (j + 17)) % 997) / 997 for j in range(1, dimensions)]
    rng = random.Random(key)
    return [1.0] + [2.0 * rng.random() - 1.0 for _ in range(dimensions - 1)]


def reference_distance(key, dimensions, fixture):
    # Independently compute cosine to [1,0,...] from the stored float32 values.
    # The engine may accumulate in float32; validation allows that roundoff.
    values = [struct.unpack("<f", struct.pack("<f", x))[0]
              for x in vector_for(key, dimensions, fixture)]
    return 1.0 - 1.0 / math.sqrt(math.fsum(x * x for x in values))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/romedb-cli"))
    parser.add_argument("--rows", type=int, default=10000)
    parser.add_argument("--dimensions", type=int, default=16)
    parser.add_argument("--samples", type=int, default=7)
    parser.add_argument("--fixture", choices=["cyclic", "seeded"], default="cyclic")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not 100 <= args.rows <= 1000000 or not 2 <= args.dimensions <= 1024 or not 1 <= args.samples <= 100:
        parser.error("rows must be 100..1000000, dimensions 2..1024, samples 1..100")
    binary = args.binary.resolve(strict=True)
    root = Path(__file__).resolve().parents[2]
    with tempfile.TemporaryDirectory(prefix="romedb-benchmark-") as directory, tempfile.TemporaryFile() as errors:
        database = Path(directory) / "benchmark.db"
        child = subprocess.Popen([str(binary), "--line", str(database)], stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, stderr=errors, bufsize=0)
        pending = b""

        def query(sql):
            nonlocal pending
            start = time.perf_counter_ns()
            remaining_input = memoryview((sql + "\n").encode())
            while remaining_input:
                written = child.stdin.write(remaining_input)
                if not written:
                    raise BrokenPipeError("CLI input closed")
                remaining_input = remaining_input[written:]
            child.stdin.flush()
            deadline = time.monotonic() + 600
            while b"\n" not in pending:
                remaining = deadline - time.monotonic()
                if remaining <= 0 or not select.select([child.stdout], [], [], min(remaining, 10))[0]:
                    if remaining <= 0:
                        raise TimeoutError("benchmark statement exceeded 600 seconds")
                    continue
                chunk = os.read(child.stdout.fileno(), 65536)
                if not chunk:
                    errors.seek(0)
                    raise RuntimeError(errors.read().decode(errors="replace"))
                pending += chunk
            line, pending = pending.split(b"\n", 1)
            result = json.loads(line)
            elapsed = (time.perf_counter_ns() - start) / 1000000
            if "error" in result:
                raise RuntimeError(result["error"])
            return result, elapsed

        def peak_rss():
            path = Path(f"/proc/{child.pid}/status")
            if not path.exists():
                return None
            for line in path.read_text().splitlines():
                if line.startswith("VmHWM:"):
                    return int(line.split()[1]) * 1024
            return None

        def measure(name, sql, validate):
            print(f"measuring {name}", flush=True)
            plan, _ = query("EXPLAIN QUERY PLAN " + sql)
            warmup, _ = query(".profile " + sql)
            validate(warmup)
            times = []
            counters = []
            for _ in range(args.samples):
                result, elapsed = query(".profile " + sql)
                validate(result)
                counters.append(result["profile"])
                times.append(elapsed)
            return {"name": name, "sql": sql, "milliseconds": times,
                    "median_ms": statistics.median(times),
                    "p95_ms": sorted(times)[math.ceil(0.95 * len(times)) - 1],
                    "process_peak_rss_bytes": peak_rss(), "plan": plan["rows"],
                    "engine_rows_read": counters[0]["rows_read"],
                    "engine_fullscan_steps": counters[0]["fullscan_steps"],
                    "engine_counters": counters}

        try:
            query("CREATE TABLE docs")
            query(f"DEFINE FIELD embedding ON docs TYPE vector<{args.dimensions}> REQUIRED")
            load_start = time.perf_counter()
            query("BEGIN")
            for offset in range(0, args.rows, 100):
                values = []
                for key in range(offset, min(offset + 100, args.rows)):
                    vector = vector_for(key, args.dimensions, args.fixture)
                    values.append(f"(type::record('docs',{key}),{key % 100},'document {key}',vector32('{json.dumps(vector)}'))")
                query("INSERT INTO docs (id,group_no,title,embedding) VALUES " + ",".join(values))
                if (offset + 100) % 10000 == 0:
                    print(f"loaded {min(offset + 100, args.rows)} documents", flush=True)
            query("COMMIT")
            load_seconds = time.perf_counter() - load_start
            expected = len(range(7, args.rows, 100))

            def count(result):
                assert result["rows"] == [[{"type": "Integer", "value": expected}]], result

            reports = [measure("unindexed_filter", "SELECT count(*) FROM docs WHERE group_no=7", count)]
            print("building docs_group index", flush=True)
            index_start = time.perf_counter()
            query("CREATE INDEX docs_group ON docs(group_no)")
            index_seconds = time.perf_counter() - index_start
            reports.append(measure("indexed_filter", "SELECT count(*) FROM docs WHERE group_no=7", count))
            assert "SEARCH" in json.dumps(reports[-1]["plan"]).upper(), reports[-1]["plan"]
            assert "docs_group" in json.dumps(reports[-1]["plan"]), reports[-1]["plan"]
            target = json.dumps([1.0] + [0.0] * (args.dimensions - 1))

            print("computing independent float32 cosine reference", flush=True)
            reference = heapq.nsmallest(10, ((reference_distance(key, args.dimensions, args.fixture), key)
                                            for key in range(args.rows)))
            tolerance = 2e-6
            cutoff = reference[-1][0]
            required = {key for distance, key in reference if distance < cutoff - tolerance}

            def nearest(result):
                assert len(result["rows"]) == 10, result
                assert result["rows"][0][0]["value"] == 0, result
                assert abs(result["rows"][0][1]["value"]) < 1e-5, result
                distances = [row[1]["value"] for row in result["rows"]]
                assert distances == sorted(distances), result
                keys = [row[0]["value"] for row in result["rows"]]
                assert len(set(keys)) == 10 and all(0 <= key < args.rows for key in keys), result
                assert list(zip(distances, keys)) == sorted(zip(distances, keys)), result
                assert required.issubset(keys), (result, reference)
                for key, distance in zip(keys, distances):
                    expected_distance = reference_distance(key, args.dimensions, args.fixture)
                    assert abs(distance - expected_distance) <= tolerance, (key, distance, expected_distance)
                    assert expected_distance <= cutoff + tolerance, (key, expected_distance, cutoff)

            reports.append(measure("exact_vector_top10", f"SELECT record::id(id) AS key,vector_distance_cos(embedding,vector32('{target}')) AS distance FROM docs ORDER BY distance,id LIMIT 10", nearest))
            query("PRAGMA wal_checkpoint(TRUNCATE)")
            child.stdin.close()
            assert child.wait(timeout=30) == 0
            report = {"format": "romedb-benchmark-v1", "created_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                      "platform": platform.platform(), "machine": platform.machine(),
                      "binary": str(binary), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                      "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
                      "worktree_status": subprocess.check_output(["git", "status", "--short"], cwd=root, text=True).splitlines(),
                      "rows": args.rows, "dimensions": args.dimensions, "samples": args.samples,
                      "fixture": args.fixture + "-v1", "python_version": platform.python_version(),
                      "cosine_reference": {"top10": [{"key": key, "distance": distance} for distance, key in reference],
                                           "absolute_tolerance": tolerance, "calculation": "float64 fsum over rounded float32 coordinates"},
                      "load_seconds": load_seconds, "index_build_seconds": index_seconds,
                      "database_bytes_after_checkpoint": database.stat().st_size, "workloads": reports,
                      "measurement": "warm CLI round trip, including frontend execution and JSON transport; one warmup per workload",
                      "limitations": ["Binary build flags/profile must be recorded separately; the default binary is unoptimized.",
                                      "Linux process high-water RSS includes loading and earlier workloads, not isolated query memory.",
                                      "Engine counters cover the primary statement, excluding catalog/lowering queries and Rust decoding.",
                                      "Synthetic deterministic vectors and one process do not qualify production workloads, cold caches or concurrency."]}
            args.output.write_text(json.dumps(report, indent=2) + "\n")
            print(json.dumps({"report": str(args.output), "rows": args.rows,
                              "median_ms": {r["name"]: r["median_ms"] for r in reports}}))
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=10)
            if not child.stdin.closed:
                child.stdin.close()
            child.stdout.close()


if __name__ == "__main__":
    main()
