"""Previous-version upgrade/restore checks for a release candidate.

One table drives both bundle qualification (check-v2-bundle.py) and the dry run
before tagging (check-upgrades.py). A check applies to every candidate at or
after its first version, so a new release only adds a row for its predecessor.
"""
from dataclasses import dataclass
from pathlib import Path


def parse(version):
    parts = version.split(".")
    assert len(parts) == 3 and all(p.isdigit() for p in parts), f"not a release version: {version}"
    return tuple(int(p) for p in parts)


def at_least(version, first):
    return parse(version) >= parse(first)


@dataclass(frozen=True)
class Upgrade:
    label: str
    previous: str
    script: str
    first: str
    per_node: bool = True


# RomeDB 0.2.9 is the first release under the RomeDB name and the __romedb_
# storage prefix. FastDB 1.x and 2.x databases are not upgradeable, so no
# earlier package applies. Add a row for each RomeDB predecessor from 0.2.10 on,
# using check-v251-upgrade.cjs (the generic previous-package -> candidate check).
UPGRADES = [
    Upgrade("v029", "0.2.9", "check-v251-upgrade.cjs", "0.2.10"),
    Upgrade("v0210", "0.2.10", "check-v251-upgrade.cjs", "0.2.11"),
    Upgrade("v0211", "0.2.11", "check-v251-upgrade.cjs", "0.2.12"),
    Upgrade("v0213", "0.2.13", "check-v251-upgrade.cjs", "0.2.14"),
    Upgrade("v0214", "0.2.14", "check-v251-upgrade.cjs", "0.2.15"),
    Upgrade("v0215", "0.2.15", "check-v251-upgrade.cjs", "0.2.16"),
    Upgrade("v0216", "0.2.16", "check-v251-upgrade.cjs", "0.2.17"),
    Upgrade("v0217", "0.2.17", "check-v251-upgrade.cjs", "0.2.18"),
    Upgrade("v0218", "0.2.18", "check-v251-upgrade.cjs", "0.2.19"),
]

# 0.2.13 changed the stored value prefix from FDB to RDB and does not open files
# from 0.2.12 or earlier, so no package before a break applies to a candidate
# at or after it.
FORMAT_BREAKS = ["0.2.13"]


def applicable(candidate):
    breaks = [b for b in FORMAT_BREAKS if at_least(candidate, b)]
    floor = max(breaks, key=parse) if breaks else "0.0.0"
    return [
        u
        for u in UPGRADES
        if at_least(candidate, u.first)
        and parse(u.previous) < parse(candidate)
        and at_least(u.previous, floor)
    ]


def previous_packages(values, candidate):
    """Parse repeated VERSION=PATH options and require every applicable package."""
    import json
    packages = {}
    for value in values or []:
        version, separator, path = value.partition("=")
        assert separator, f"expected VERSION=PATH, got {value}"
        resolved = Path(path).resolve()
        found = json.loads((resolved / "package.json").read_text())["version"]
        assert found == version, f"{resolved} is {found}, not {version}"
        packages[version] = resolved
    missing = sorted({u.previous for u in applicable(candidate)} - packages.keys(), key=parse)
    assert not missing, f"{candidate} requires previous Node packages: {', '.join(missing)}"
    return packages


def evidence_name(upgrade, index):
    if not upgrade.per_node:
        return "upgrade-fixture", f"{upgrade.label}-upgrade-restore"
    return f"{upgrade.label}-upgrade-fixture-{index}", f"{upgrade.label}-upgrade-restore-{index}"


def run_upgrades(run, root, candidate, candidate_package, packages, nodes, evidence, before_node=None):
    """Run every applicable check: single-node ones with the first runtime, the
    others once per runtime. `run(name, command)` executes and logs one group."""
    scripts = Path(root) / "romedb/scripts"
    for upgrade in applicable(candidate):
        if not upgrade.per_node:
            fixture, name = evidence_name(upgrade, 0)
            run(name, [nodes[0], str(scripts / upgrade.script), str(packages[upgrade.previous]),
                       str(candidate_package), str(Path(evidence) / fixture)])
    for index, node in enumerate(nodes):
        if before_node:
            before_node(index, node)
        for upgrade in applicable(candidate):
            if upgrade.per_node:
                fixture, name = evidence_name(upgrade, index)
                run(name, [node, str(scripts / upgrade.script), str(packages[upgrade.previous]),
                           str(candidate_package), str(Path(evidence) / fixture)])
