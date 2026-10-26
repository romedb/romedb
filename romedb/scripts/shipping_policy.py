"""One checked build policy shared by the builder and artifact verifier."""
import os
import tomllib

PROFILE = "romedb-production"
POLICY = {
    "inherits": "dev", "opt-level": 3, "debug": "line-tables-only",
    "debug-assertions": True, "overflow-checks": True, "panic": "unwind",
    "incremental": False, "codegen-units": 16, "lto": "thin",
}


def profile_toml():
    def value(item):
        if isinstance(item, bool):
            return str(item).lower()
        return repr(item).replace("'", '"') if isinstance(item, str) else str(item)
    return f"\n[profile.{PROFILE}]\n" + "".join(f"{key} = {value(item)}\n" for key, item in POLICY.items())


def check_profile(cargo_toml):
    actual = tomllib.loads(cargo_toml)["profile"][PROFILE]
    if actual != POLICY:
        raise ValueError(f"Shipping profile differs from the checked policy: {actual}")


def build_environment():
    environment = dict(os.environ)
    # Cargo's profile variables override source configuration. Supply all policy
    # fields ourselves so a developer's shell cannot silently change shipping.
    for key, value in POLICY.items():
        if key != "inherits":
            name = f"CARGO_PROFILE_ROMEDB_PRODUCTION_{key.upper().replace('-', '_')}"
            environment[name] = str(value).lower() if isinstance(value, bool) else str(value)
    for key in environment:
        if environment[key] and (key in {"RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS"}
                                 or key.startswith("CARGO_TARGET_") and key.endswith("_RUSTFLAGS")):
            raise ValueError(f"Unset {key} for a reproducible shipping build; checked-in Cargo flags remain active")
    return environment


def check_artifact(profile):
    expected = {"opt_level": "3", "debug_assertions": True, "overflow_checks": True, "test": False}
    if any(profile.get(key) != value for key, value in expected.items()):
        raise ValueError(f"Cargo artifact does not satisfy shipping policy: {profile}")


def check_manifest(manifest):
    if manifest.get("buildProfile") != PROFILE or manifest.get("rustProfilePolicy") != POLICY:
        raise ValueError("Bundle is not built with the checked RomeDB production profile")
    if manifest.get("csharpConfiguration") != "Release":
        raise ValueError("Bundle C# package must use the Release configuration")
    for name in ("cargoBuildCommand", "pythonBuildCommand"):
        command = manifest.get(name, [])
        if "--profile" not in command or command[command.index("--profile") + 1:command.index("--profile") + 2] != [PROFILE] or "--release" in command:
            raise ValueError(f"Bundle build command does not select the shipping profile: {name}")
    artifacts = manifest.get("cargoArtifacts", {})
    if set(artifacts) != {"romedb-cli", "romedb_node", "romedb_c", "_native"}:
        raise ValueError("Bundle is missing native Cargo artifact provenance")
    for artifact in artifacts.values():
        check_artifact(artifact["profile"])
        if not artifact.get("sha256") or PROFILE not in artifact.get("path", "").split("/"):
            raise ValueError("Invalid Cargo artifact identity")


def check_security_review(receipt, lockfile_sha256):
    if receipt.get("cargo_lock_sha256") != lockfile_sha256:
        raise ValueError("Dependency security review does not match the shipping Cargo.lock")
    if receipt.get("unresolved_count") != 0:
        raise ValueError("Dependency security review has unresolved or missing findings status")
