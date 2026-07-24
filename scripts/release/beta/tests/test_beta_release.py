#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


REPO_ROOT = Path(__file__).resolve().parents[4]
RELEASE_TOOL = REPO_ROOT / "scripts/release/beta/release.py"
VERIFY_SCRIPT = REPO_ROOT / "scripts/release/beta/verify-release.sh"
WORKFLOW = REPO_ROOT / ".github/workflows/package-beta.yml"
TRUST_POLICY = REPO_ROOT / "scripts/release/beta/trust-policy.json"
SCHEMA = REPO_ROOT / "schemas/release-manifest/beta-v1.json"

TARGETS = {
    "aarch64-apple-darwin": "darwin-arm64",
    "x86_64-apple-darwin": "darwin-x64",
    "x86_64-unknown-linux-gnu": "linux-x64",
}


def run(*arguments: str, check: bool = True, env: dict[str, str] | None = None):
    return subprocess.run(
        arguments,
        cwd=REPO_ROOT,
        check=check,
        text=True,
        capture_output=True,
        env=env,
    )


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(65536), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_artifact(
    root: Path,
    rust_target: str,
    package_target: str,
    *,
    signed: bool,
) -> None:
    artifact_dir = root / package_target
    artifact_dir.mkdir(parents=True, exist_ok=True)
    binary = artifact_dir / "compresso"
    binary.write_bytes(f"binary for {rust_target}\n".encode())
    binary.chmod(0o755)
    sbom = artifact_dir / "compresso.cdx.json"
    sbom.write_text(
        json.dumps(
            {
                "bomFormat": "CycloneDX",
                "specVersion": "1.5",
                "version": 1,
                "components": [],
            },
            sort_keys=True,
        )
        + "\n"
    )
    signing_kind = "apple_codesign" if "apple" in rust_target else "sigstore_manifest"
    signing_identity = "Developer ID Application: Compresso Test (TESTTEAM)" if signed else ""
    metadata = {
        "schema_version": 1,
        "rust_target": rust_target,
        "package_target": package_target,
        "binary": {
            "path": "compresso",
            "sha256": sha256(binary),
            "size": binary.stat().st_size,
        },
        "sbom": {
            "path": "compresso.cdx.json",
            "sha256": sha256(sbom),
            "size": sbom.stat().st_size,
            "format": "cyclonedx-json",
        },
        "native_signing": {
            "kind": signing_kind,
            "verified": signed or "linux" in rust_target,
            "identity": signing_identity,
        },
    }
    (artifact_dir / "artifact-metadata.json").write_text(
        json.dumps(metadata, indent=2, sort_keys=True) + "\n"
    )


class BetaReleaseTests(unittest.TestCase):
    maxDiff = None

    def setUp(self) -> None:
        self.temp_dir = Path(tempfile.mkdtemp(prefix="compresso-beta-test-"))
        self.addCleanup(lambda: shutil.rmtree(self.temp_dir, ignore_errors=True))

    def create_release(self) -> tuple[Path, Path]:
        artifacts = self.temp_dir / "artifacts"
        for rust_target, package_target in TARGETS.items():
            write_artifact(
                artifacts,
                rust_target,
                package_target,
                signed="apple" in rust_target,
            )
        manifest = self.temp_dir / "beta-release-manifest.json"
        run(
            str(RELEASE_TOOL),
            "assemble",
            "--artifacts-root",
            str(artifacts),
            "--package-version",
            "0.1.0-beta.0",
            "--engine-version",
            "0.1.0",
            "--source-repository",
            "yogi100x/compresso",
            "--source-commit",
            "a" * 40,
            "--workflow-ref",
            "yogi100x/compresso/.github/workflows/package-beta.yml@refs/tags/cli-v0.1.0-beta.0",
            "--run-id",
            "123456",
            "--run-attempt",
            "1",
            "--output",
            str(manifest),
        )
        return artifacts, manifest

    def verify(
        self,
        artifacts: Path,
        manifest: Path,
        *,
        expected_version: str = "0.1.0-beta.0",
        expected_provenance: str = "github-actions:yogi100x/compresso:123456:1",
        check: bool = True,
    ):
        return run(
            str(RELEASE_TOOL),
            "verify",
            "--artifacts-root",
            str(artifacts),
            "--manifest",
            str(manifest),
            "--expected-package-version",
            expected_version,
            "--expected-provenance-identity",
            expected_provenance,
            check=check,
        )

    def test_beta_matrix_exact_three_targets(self) -> None:
        workflow = WORKFLOW.read_text()
        matrix_targets = {
            line.split(":", 1)[1].strip()
            for line in workflow.splitlines()
            if line.strip().startswith("- rust_target:")
        }
        self.assertEqual(matrix_targets, set(TARGETS))
        self.assertNotIn("windows", workflow.lower())
        self.assertIn("test_beta_matrix_exact_three_targets", workflow)

        artifacts = self.temp_dir / "stable-artifacts"
        for rust_target, package_target in TARGETS.items():
            write_artifact(
                artifacts,
                rust_target,
                package_target,
                signed="apple" in rust_target,
            )
        stable = run(
            str(RELEASE_TOOL),
            "assemble",
            "--artifacts-root",
            str(artifacts),
            "--package-version",
            "0.1.0",
            "--engine-version",
            "0.1.0",
            "--source-repository",
            "yogi100x/compresso",
            "--source-commit",
            "a" * 40,
            "--workflow-ref",
            "yogi100x/compresso/.github/workflows/package-beta.yml@refs/tags/cli-v0.1.0",
            "--run-id",
            "123456",
            "--run-attempt",
            "1",
            "--output",
            str(self.temp_dir / "stable.json"),
            check=False,
        )
        self.assertNotEqual(stable.returncode, 0)
        self.assertIn("-beta.N", stable.stderr)

    def test_beta_manifest_signature_digest_and_provenance_reject_tamper(self) -> None:
        artifacts, manifest = self.create_release()
        self.verify(artifacts, manifest)

        schema = json.loads(SCHEMA.read_text())
        self.assertEqual(schema["$schema"], "https://json-schema.org/draft/2020-12/schema")
        self.assertFalse(schema["additionalProperties"])

        binary = artifacts / "darwin-arm64/compresso"
        binary.write_bytes(binary.read_bytes() + b"tamper")
        result = self.verify(artifacts, manifest, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("mismatch", result.stderr)

        write_artifact(
            artifacts,
            "aarch64-apple-darwin",
            "darwin-arm64",
            signed=True,
        )
        wrong_version = self.verify(
            artifacts,
            manifest,
            expected_version="0.1.0-beta.1",
            check=False,
        )
        self.assertNotEqual(wrong_version.returncode, 0)
        self.assertIn("package version mismatch", wrong_version.stderr)

        wrong_provenance = self.verify(
            artifacts,
            manifest,
            expected_provenance="github-actions:attacker/repository:123456:1",
            check=False,
        )
        self.assertNotEqual(wrong_provenance.returncode, 0)
        self.assertIn("provenance identity mismatch", wrong_provenance.stderr)

        wrong_target_document = json.loads(manifest.read_text())
        wrong_target_document["artifacts"][0]["package_target"] = "linux-x64"
        wrong_target_manifest = self.temp_dir / "wrong-target-manifest.json"
        wrong_target_manifest.write_text(
            json.dumps(wrong_target_document, indent=2, sort_keys=True) + "\n"
        )
        wrong_target = self.verify(
            artifacts,
            wrong_target_manifest,
            check=False,
        )
        self.assertNotEqual(wrong_target.returncode, 0)
        self.assertIn("target mapping mismatch", wrong_target.stderr)

        fake_bin = self.temp_dir / "fake-bin"
        fake_bin.mkdir()
        fake_cosign = fake_bin / "cosign"
        fake_cosign.write_text(
            "#!/bin/sh\n"
            "arguments=\" $* \"\n"
            "case \"$arguments\" in *' --certificate-identity "
            "https://github.com/yogi100x/compresso/.github/workflows/package-beta.yml@refs/tags/cli-v0.1.0-beta.0 '*) ;; *) exit 20 ;; esac\n"
            "case \"$arguments\" in *' --certificate-github-workflow-repository "
            "yogi100x/compresso '*) ;; *) exit 21 ;; esac\n"
            "case \"$arguments\" in *' --certificate-github-workflow-ref "
            "refs/tags/cli-v0.1.0-beta.0 '*) ;; *) exit 22 ;; esac\n"
            "case \"$arguments\" in *' --certificate-github-workflow-sha "
            + "a" * 40
            + " '*) ;; *) exit 23 ;; esac\n"
            "case \"$arguments\" in *' --certificate-github-workflow-trigger "
            "workflow_dispatch '*) exit 0 ;; *) exit 24 ;; esac\n"
        )
        fake_cosign.chmod(0o755)
        bundle = self.temp_dir / "manifest.sigstore.json"
        bundle.write_text("{}\n")
        environment = os.environ.copy()
        environment["PATH"] = f"{fake_bin}:{environment['PATH']}"
        run(
            str(VERIFY_SCRIPT),
            "--manifest",
            str(manifest),
            "--bundle",
            str(bundle),
            "--artifacts-root",
            str(artifacts),
            "--package-version",
            "0.1.0-beta.0",
            "--provenance-identity",
            "github-actions:yogi100x/compresso:123456:1",
            env=environment,
        )

        policy = json.loads(TRUST_POLICY.read_text())
        policy["certificate_identity_prefix"] = "https://github.com/attacker/repository/"
        bad_policy = self.temp_dir / "untrusted-policy.json"
        bad_policy.write_text(json.dumps(policy))
        untrusted = run(
            str(VERIFY_SCRIPT),
            "--manifest",
            str(manifest),
            "--bundle",
            str(bundle),
            "--artifacts-root",
            str(artifacts),
            "--package-version",
            "0.1.0-beta.0",
            "--provenance-identity",
            "github-actions:yogi100x/compresso:123456:1",
            "--trust-policy",
            str(bad_policy),
            check=False,
            env=environment,
        )
        self.assertNotEqual(untrusted.returncode, 0)

    def test_beta_artifact_allowlist_rejects_each_private_class(self) -> None:
        artifacts, _ = self.create_release()
        artifact = artifacts / "linux-x64"
        run(
            str(RELEASE_TOOL),
            "check-artifact",
            "--artifact-dir",
            str(artifact),
            "--package-target",
            "linux-x64",
        )

        forbidden = [
            "compresso.rs",
            "prompt.txt",
            "fixture.json",
            "compresso.debug",
            "source.map",
            "signing-key.pem",
            "corpus.toml",
            ".env",
        ]
        for name in forbidden:
            with self.subTest(name=name):
                candidate = artifact / name
                candidate.write_text("private material\n")
                result = run(
                    str(RELEASE_TOOL),
                    "check-artifact",
                    "--artifact-dir",
                    str(artifact),
                    "--package-target",
                    "linux-x64",
                    check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                candidate.unlink()

        extra = artifact / "unlisted.bin"
        extra.write_bytes(b"extra")
        result = run(
            str(RELEASE_TOOL),
            "check-artifact",
            "--artifact-dir",
            str(artifact),
            "--package-target",
            "linux-x64",
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpected payload file", result.stderr)

    def test_beta_symbols_private_and_native_signing_policy(self) -> None:
        cargo = (REPO_ROOT / "Cargo.toml").read_text()
        self.assertIn("[profile.dist]", cargo)
        self.assertIn('split-debuginfo = "packed"', cargo)
        self.assertIn('strip = "symbols"', cargo)

        workflow = WORKFLOW.read_text()
        self.assertIn("Upload private debug symbols", workflow)
        self.assertIn("private-symbols-${{ matrix.rust_target }}", workflow)
        self.assertIn("codesign --verify --strict", workflow)
        self.assertIn("APPLE_CERTIFICATE_P12_BASE64", workflow)
        self.assertIn("cosign sign-blob", workflow)
        self.assertIn("cargo cyclonedx", workflow)
        self.assertIn("cargo test --workspace --no-fail-fast", workflow)
        self.assertIn("cargo run -p compresso-eval --profile dist", workflow)
        self.assertIn("Scan public artifact payload for secrets", workflow)
        self.assertIn("test_beta_symbols_private_and_native_signing_policy", workflow)

        artifacts, manifest = self.create_release()
        document = json.loads(manifest.read_text())
        by_target = {
            artifact["rust_target"]: artifact for artifact in document["artifacts"]
        }
        for target in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
            self.assertEqual(
                by_target[target]["native_signing"]["kind"],
                "apple_codesign",
            )
            self.assertTrue(by_target[target]["native_signing"]["verified"])
            self.assertTrue(by_target[target]["native_signing"]["identity"])
        self.assertEqual(
            by_target["x86_64-unknown-linux-gnu"]["native_signing"]["kind"],
            "sigstore_manifest",
        )
        self.assertTrue(
            by_target["x86_64-unknown-linux-gnu"]["native_signing"]["verified"]
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
