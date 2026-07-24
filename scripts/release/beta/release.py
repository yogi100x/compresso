#!/usr/bin/env python3
"""Create and verify Compresso's private beta release manifest.

This tool deliberately uses only the Python standard library so the private
build and the public import can run the same verifier without installing
project dependencies.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
from typing import Any


TARGETS = {
    "aarch64-apple-darwin": "darwin-arm64",
    "x86_64-apple-darwin": "darwin-x64",
    "x86_64-unknown-linux-gnu": "linux-x64",
}
PACKAGE_NAME = "@compresso/cli"
ALLOWED_ARTIFACT_FILES = {
    "compresso",
    "compresso.cdx.json",
    "artifact-metadata.json",
}
SHA256_PATTERN = re.compile(r"^[0-9a-f]{64}$")
VERSION_PATTERN = re.compile(
    r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$"
)
BETA_PACKAGE_VERSION_PATTERN = re.compile(
    r"^[0-9]+\.[0-9]+\.[0-9]+-beta\.[0-9]+$"
)
COMMIT_PATTERN = re.compile(r"^[0-9a-f]{40}$")


class ReleaseValidationError(Exception):
    pass


def fail(message: str) -> None:
    raise ReleaseValidationError(message)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"invalid JSON at {path}: {error}")
    if not isinstance(value, dict):
        fail(f"expected a JSON object at {path}")
    return value


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True, separators=(",", ": ")) + "\n",
        encoding="utf-8",
    )


def require_exact_keys(
    value: dict[str, Any],
    expected: set[str],
    location: str,
) -> None:
    actual = set(value)
    if actual != expected:
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        fail(f"{location} keys mismatch (missing={missing}, extra={extra})")


def require_string(value: Any, location: str, *, nonempty: bool = True) -> str:
    if not isinstance(value, str) or (nonempty and not value):
        fail(f"{location} must be {'a non-empty ' if nonempty else 'a '}string")
    return value


def require_positive_integer(value: Any, location: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        fail(f"{location} must be a positive integer")
    return value


def validate_digest(value: Any, location: str) -> str:
    digest = require_string(value, location)
    if not SHA256_PATTERN.fullmatch(digest):
        fail(f"{location} must be a lowercase SHA-256 digest")
    return digest


def validate_version(value: Any, location: str) -> str:
    version = require_string(value, location)
    if not VERSION_PATTERN.fullmatch(version):
        fail(f"{location} is not a supported semantic version")
    return version


def validate_beta_package_version(value: Any, location: str) -> str:
    version = require_string(value, location)
    if not BETA_PACKAGE_VERSION_PATTERN.fullmatch(version):
        fail(f"{location} must use the numeric -beta.N prerelease form")
    return version


def regular_files(root: Path) -> set[str]:
    if not root.is_dir() or root.is_symlink():
        fail(f"artifact directory is missing or unsafe: {root}")
    files: set[str] = set()
    for candidate in root.rglob("*"):
        relative = candidate.relative_to(root).as_posix()
        if candidate.is_symlink():
            fail(f"artifact payload contains a symlink: {relative}")
        if candidate.is_dir():
            continue
        if not candidate.is_file():
            fail(f"artifact payload contains a non-regular file: {relative}")
        files.add(relative)
    return files


def check_artifact_payload(artifact_dir: Path, package_target: str) -> None:
    if package_target not in TARGETS.values():
        fail(f"unsupported package target: {package_target}")
    files = regular_files(artifact_dir)
    extra = sorted(files - ALLOWED_ARTIFACT_FILES)
    missing = sorted(ALLOWED_ARTIFACT_FILES - files)
    if extra:
        fail(f"unexpected payload file: {extra[0]}")
    if missing:
        fail(f"artifact payload is missing: {missing[0]}")

    binary = artifact_dir / "compresso"
    if not binary.stat().st_mode & 0o111:
        fail(f"artifact binary is not executable: {binary}")


def validate_file_record(
    value: Any,
    location: str,
    *,
    expected_path: str,
    artifact_dir: Path | None,
) -> dict[str, Any]:
    if not isinstance(value, dict):
        fail(f"{location} must be an object")
    require_exact_keys(value, {"path", "sha256", "size"}, location)
    path = require_string(value["path"], f"{location}.path")
    if path != expected_path:
        fail(f"{location}.path must be {expected_path}")
    digest = validate_digest(value["sha256"], f"{location}.sha256")
    size = require_positive_integer(value["size"], f"{location}.size")
    if artifact_dir is not None:
        subject = artifact_dir / path
        if not subject.is_file() or subject.is_symlink():
            fail(f"{location} file is missing or unsafe: {subject}")
        if subject.stat().st_size != size:
            fail(f"{location} size mismatch for {subject}")
        if sha256_file(subject) != digest:
            fail(f"{location} digest mismatch for {subject}")
    return {"path": path, "sha256": digest, "size": size}


def validate_sbom_record(
    value: Any,
    artifact_dir: Path | None,
) -> dict[str, Any]:
    if not isinstance(value, dict):
        fail("artifact.sbom must be an object")
    require_exact_keys(value, {"path", "sha256", "size", "format"}, "artifact.sbom")
    base = validate_file_record(
        {key: value[key] for key in ("path", "sha256", "size")},
        "artifact.sbom",
        expected_path="compresso.cdx.json",
        artifact_dir=artifact_dir,
    )
    if value["format"] != "cyclonedx-json":
        fail("artifact.sbom.format must be cyclonedx-json")
    if artifact_dir is not None:
        sbom = load_json(artifact_dir / base["path"])
        if sbom.get("bomFormat") != "CycloneDX":
            fail("artifact SBOM is not CycloneDX JSON")
    return {**base, "format": "cyclonedx-json"}


def validate_native_signing(
    value: Any,
    rust_target: str,
) -> dict[str, Any]:
    if not isinstance(value, dict):
        fail("artifact.native_signing must be an object")
    require_exact_keys(
        value,
        {"kind", "verified", "identity"},
        "artifact.native_signing",
    )
    kind = require_string(value["kind"], "artifact.native_signing.kind")
    if not isinstance(value["verified"], bool) or not value["verified"]:
        fail("artifact.native_signing.verified must be true")
    identity = require_string(
        value["identity"],
        "artifact.native_signing.identity",
        nonempty=False,
    )
    if rust_target.endswith("apple-darwin"):
        if kind != "apple_codesign":
            fail(f"macOS target {rust_target} requires apple_codesign")
        if not identity:
            fail(f"macOS target {rust_target} requires a signing identity")
    else:
        if kind != "sigstore_manifest":
            fail(f"Linux target {rust_target} requires sigstore_manifest signing")
        if identity:
            fail("Linux manifest signing identity is supplied by the Sigstore bundle")
    return {"kind": kind, "verified": True, "identity": identity}


def validate_artifact_metadata(
    value: dict[str, Any],
    *,
    artifact_dir: Path | None,
) -> dict[str, Any]:
    require_exact_keys(
        value,
        {
            "schema_version",
            "rust_target",
            "package_target",
            "binary",
            "sbom",
            "native_signing",
        },
        "artifact metadata",
    )
    if value["schema_version"] != 1:
        fail("artifact metadata schema_version must be 1")
    rust_target = require_string(value["rust_target"], "artifact.rust_target")
    package_target = require_string(value["package_target"], "artifact.package_target")
    if TARGETS.get(rust_target) != package_target:
        fail(f"target mapping mismatch: {rust_target} -> {package_target}")
    binary = validate_file_record(
        value["binary"],
        "artifact.binary",
        expected_path="compresso",
        artifact_dir=artifact_dir,
    )
    sbom = validate_sbom_record(value["sbom"], artifact_dir)
    signing = validate_native_signing(value["native_signing"], rust_target)
    return {
        "schema_version": 1,
        "rust_target": rust_target,
        "package_target": package_target,
        "binary": binary,
        "sbom": sbom,
        "native_signing": signing,
    }


def command_write_metadata(arguments: argparse.Namespace) -> None:
    artifact_dir = arguments.artifact_dir.resolve()
    rust_target = arguments.rust_target
    package_target = arguments.package_target
    if TARGETS.get(rust_target) != package_target:
        fail(f"target mapping mismatch: {rust_target} -> {package_target}")
    binary = artifact_dir / "compresso"
    sbom = artifact_dir / "compresso.cdx.json"
    for subject in (binary, sbom):
        if not subject.is_file() or subject.is_symlink():
            fail(f"required artifact file is missing or unsafe: {subject}")
    value = {
        "schema_version": 1,
        "rust_target": rust_target,
        "package_target": package_target,
        "binary": {
            "path": "compresso",
            "sha256": sha256_file(binary),
            "size": binary.stat().st_size,
        },
        "sbom": {
            "path": "compresso.cdx.json",
            "sha256": sha256_file(sbom),
            "size": sbom.stat().st_size,
            "format": "cyclonedx-json",
        },
        "native_signing": {
            "kind": arguments.signing_kind,
            "verified": True,
            "identity": arguments.signing_identity,
        },
    }
    validate_artifact_metadata(value, artifact_dir=artifact_dir)
    write_json(artifact_dir / "artifact-metadata.json", value)
    check_artifact_payload(artifact_dir, package_target)


def command_check_artifact(arguments: argparse.Namespace) -> None:
    artifact_dir = arguments.artifact_dir.resolve()
    check_artifact_payload(artifact_dir, arguments.package_target)
    value = load_json(artifact_dir / "artifact-metadata.json")
    validate_artifact_metadata(value, artifact_dir=artifact_dir)


def validated_metadata_directories(root: Path) -> list[tuple[Path, dict[str, Any]]]:
    if not root.is_dir() or root.is_symlink():
        fail(f"artifacts root is missing or unsafe: {root}")
    actual_directories = {
        candidate.name
        for candidate in root.iterdir()
        if candidate.is_dir() and not candidate.is_symlink()
    }
    expected_directories = set(TARGETS.values())
    if actual_directories != expected_directories:
        fail(
            "artifact target set mismatch "
            f"(expected={sorted(expected_directories)}, actual={sorted(actual_directories)})"
        )
    unexpected_root_entries = [
        candidate.name
        for candidate in root.iterdir()
        if candidate.name not in expected_directories
    ]
    if unexpected_root_entries:
        fail(f"unexpected artifact root entry: {sorted(unexpected_root_entries)[0]}")

    result = []
    for rust_target, package_target in TARGETS.items():
        artifact_dir = root / package_target
        check_artifact_payload(artifact_dir, package_target)
        metadata = validate_artifact_metadata(
            load_json(artifact_dir / "artifact-metadata.json"),
            artifact_dir=artifact_dir,
        )
        if metadata["rust_target"] != rust_target:
            fail(f"wrong Rust target in {artifact_dir}")
        result.append((artifact_dir, metadata))
    return result


def validate_provenance(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        fail("manifest.provenance must be an object")
    require_exact_keys(
        value,
        {
            "identity",
            "source_repository",
            "source_commit",
            "workflow_ref",
            "run_id",
            "run_attempt",
        },
        "manifest.provenance",
    )
    source_repository = require_string(
        value["source_repository"],
        "manifest.provenance.source_repository",
    )
    source_commit = require_string(
        value["source_commit"],
        "manifest.provenance.source_commit",
    )
    if not COMMIT_PATTERN.fullmatch(source_commit):
        fail("manifest.provenance.source_commit must be a full Git commit SHA")
    workflow_ref = require_string(value["workflow_ref"], "manifest.provenance.workflow_ref")
    run_id = require_string(value["run_id"], "manifest.provenance.run_id")
    run_attempt = require_positive_integer(
        value["run_attempt"],
        "manifest.provenance.run_attempt",
    )
    expected_identity = (
        f"github-actions:{source_repository}:{run_id}:{run_attempt}"
    )
    if value["identity"] != expected_identity:
        fail("manifest provenance identity is not derived from its source fields")
    return {
        "identity": expected_identity,
        "source_repository": source_repository,
        "source_commit": source_commit,
        "workflow_ref": workflow_ref,
        "run_id": run_id,
        "run_attempt": run_attempt,
    }


def validate_release_manifest(value: dict[str, Any]) -> dict[str, Any]:
    require_exact_keys(
        value,
        {
            "schema_version",
            "kind",
            "channel",
            "package_name",
            "package_version",
            "engine_version",
            "immutable_release_identity",
            "provenance",
            "artifacts",
        },
        "release manifest",
    )
    if value["schema_version"] != 1:
        fail("release manifest schema_version must be 1")
    if value["kind"] != "compresso_beta_release":
        fail("release manifest kind mismatch")
    if value["channel"] != "beta":
        fail("release manifest channel must be beta")
    if value["package_name"] != PACKAGE_NAME:
        fail(f"release manifest package_name must be {PACKAGE_NAME}")
    package_version = validate_beta_package_version(
        value["package_version"],
        "manifest.package_version",
    )
    engine_version = validate_version(
        value["engine_version"],
        "manifest.engine_version",
    )
    provenance = validate_provenance(value["provenance"])
    expected_release_identity = (
        f"{provenance['identity']}:{package_version}:{provenance['source_commit']}"
    )
    if value["immutable_release_identity"] != expected_release_identity:
        fail("immutable release identity mismatch")
    artifacts = value["artifacts"]
    if not isinstance(artifacts, list) or len(artifacts) != len(TARGETS):
        fail("release manifest must contain exactly three artifacts")
    validated_artifacts = []
    seen_targets: set[str] = set()
    for index, artifact in enumerate(artifacts):
        if not isinstance(artifact, dict):
            fail(f"manifest.artifacts[{index}] must be an object")
        require_exact_keys(
            artifact,
            {
                "schema_version",
                "rust_target",
                "package_target",
                "binary",
                "sbom",
                "native_signing",
                "immutable_identity",
                "provenance_identity",
            },
            f"manifest.artifacts[{index}]",
        )
        metadata = validate_artifact_metadata(
            {key: value for key, value in artifact.items() if key not in {
                "immutable_identity",
                "provenance_identity",
            }},
            artifact_dir=None,
        )
        rust_target = metadata["rust_target"]
        if rust_target in seen_targets:
            fail(f"duplicate artifact target: {rust_target}")
        seen_targets.add(rust_target)
        if artifact["provenance_identity"] != provenance["identity"]:
            fail(f"artifact provenance identity mismatch for {rust_target}")
        expected_immutable = (
            f"{provenance['identity']}:{metadata['package_target']}:"
            f"{metadata['binary']['sha256']}"
        )
        if artifact["immutable_identity"] != expected_immutable:
            fail(f"artifact immutable identity mismatch for {rust_target}")
        validated_artifacts.append(
            {
                **metadata,
                "immutable_identity": expected_immutable,
                "provenance_identity": provenance["identity"],
            }
        )
    if seen_targets != set(TARGETS):
        fail("release manifest target set mismatch")
    validated_artifacts.sort(key=lambda artifact: artifact["rust_target"])
    return {
        "schema_version": 1,
        "kind": "compresso_beta_release",
        "channel": "beta",
        "package_name": PACKAGE_NAME,
        "package_version": package_version,
        "engine_version": engine_version,
        "immutable_release_identity": expected_release_identity,
        "provenance": provenance,
        "artifacts": validated_artifacts,
    }


def command_assemble(arguments: argparse.Namespace) -> None:
    package_version = validate_beta_package_version(
        arguments.package_version,
        "package version",
    )
    engine_version = validate_version(arguments.engine_version, "engine version")
    if not COMMIT_PATTERN.fullmatch(arguments.source_commit):
        fail("source commit must be a full lowercase Git commit SHA")
    if not arguments.run_id.isdigit() or int(arguments.run_id) <= 0:
        fail("run ID must be a positive integer string")
    if arguments.run_attempt <= 0:
        fail("run attempt must be positive")
    expected_workflow_suffix = (
        ".github/workflows/package-beta.yml@refs/tags/"
        f"cli-v{package_version}"
    )
    if not arguments.workflow_ref.endswith(expected_workflow_suffix):
        fail("workflow ref does not match the immutable package version tag")

    provenance_identity = (
        f"github-actions:{arguments.source_repository}:"
        f"{arguments.run_id}:{arguments.run_attempt}"
    )
    artifacts = []
    for _, metadata in validated_metadata_directories(arguments.artifacts_root.resolve()):
        artifacts.append(
            {
                **metadata,
                "immutable_identity": (
                    f"{provenance_identity}:{metadata['package_target']}:"
                    f"{metadata['binary']['sha256']}"
                ),
                "provenance_identity": provenance_identity,
            }
        )
    artifacts.sort(key=lambda artifact: artifact["rust_target"])
    manifest = {
        "schema_version": 1,
        "kind": "compresso_beta_release",
        "channel": "beta",
        "package_name": PACKAGE_NAME,
        "package_version": package_version,
        "engine_version": engine_version,
        "immutable_release_identity": (
            f"{provenance_identity}:{package_version}:{arguments.source_commit}"
        ),
        "provenance": {
            "identity": provenance_identity,
            "source_repository": arguments.source_repository,
            "source_commit": arguments.source_commit,
            "workflow_ref": arguments.workflow_ref,
            "run_id": arguments.run_id,
            "run_attempt": arguments.run_attempt,
        },
        "artifacts": artifacts,
    }
    validated = validate_release_manifest(manifest)
    write_json(arguments.output.resolve(), validated)


def command_verify(arguments: argparse.Namespace) -> None:
    manifest = validate_release_manifest(load_json(arguments.manifest.resolve()))
    if manifest["package_version"] != arguments.expected_package_version:
        fail(
            "package version mismatch "
            f"(expected={arguments.expected_package_version}, "
            f"actual={manifest['package_version']})"
        )
    if (
        manifest["provenance"]["identity"]
        != arguments.expected_provenance_identity
    ):
        fail(
            "provenance identity mismatch "
            f"(expected={arguments.expected_provenance_identity}, "
            f"actual={manifest['provenance']['identity']})"
        )

    actual_metadata = {
        metadata["rust_target"]: metadata
        for _, metadata in validated_metadata_directories(
            arguments.artifacts_root.resolve()
        )
    }
    for artifact in manifest["artifacts"]:
        rust_target = artifact["rust_target"]
        metadata = actual_metadata[rust_target]
        for field in ("package_target", "binary", "sbom", "native_signing"):
            if artifact[field] != metadata[field]:
                fail(f"manifest {field} mismatch for {rust_target}")


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser()
    subparsers = result.add_subparsers(dest="command", required=True)

    write_metadata = subparsers.add_parser("write-metadata")
    write_metadata.add_argument("--artifact-dir", type=Path, required=True)
    write_metadata.add_argument("--rust-target", required=True)
    write_metadata.add_argument("--package-target", required=True)
    write_metadata.add_argument(
        "--signing-kind",
        choices=("apple_codesign", "sigstore_manifest"),
        required=True,
    )
    write_metadata.add_argument("--signing-identity", default="")
    write_metadata.set_defaults(handler=command_write_metadata)

    check_artifact = subparsers.add_parser("check-artifact")
    check_artifact.add_argument("--artifact-dir", type=Path, required=True)
    check_artifact.add_argument("--package-target", required=True)
    check_artifact.set_defaults(handler=command_check_artifact)

    assemble = subparsers.add_parser("assemble")
    assemble.add_argument("--artifacts-root", type=Path, required=True)
    assemble.add_argument("--package-version", required=True)
    assemble.add_argument("--engine-version", required=True)
    assemble.add_argument("--source-repository", required=True)
    assemble.add_argument("--source-commit", required=True)
    assemble.add_argument("--workflow-ref", required=True)
    assemble.add_argument("--run-id", required=True)
    assemble.add_argument("--run-attempt", type=int, required=True)
    assemble.add_argument("--output", type=Path, required=True)
    assemble.set_defaults(handler=command_assemble)

    verify = subparsers.add_parser("verify")
    verify.add_argument("--artifacts-root", type=Path, required=True)
    verify.add_argument("--manifest", type=Path, required=True)
    verify.add_argument("--expected-package-version", required=True)
    verify.add_argument("--expected-provenance-identity", required=True)
    verify.set_defaults(handler=command_verify)
    return result


def main() -> int:
    arguments = parser().parse_args()
    try:
        arguments.handler(arguments)
    except ReleaseValidationError as error:
        print(f"error[release_validation]: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
