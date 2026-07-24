#!/bin/sh

set -eu

script_dir=$(CDPATH='' cd -P "$(dirname "$0")" && pwd)
release_tool="$script_dir/release.py"
trust_policy="$script_dir/trust-policy.json"
manifest=
bundle=
artifacts_root=
package_version=
provenance_identity=

fail() {
  printf 'verify-release: %s\n' "$*" >&2
  exit 1
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --manifest)
      [ "$#" -ge 2 ] || fail 'missing value for --manifest'
      manifest=$2
      shift 2
      ;;
    --bundle)
      [ "$#" -ge 2 ] || fail 'missing value for --bundle'
      bundle=$2
      shift 2
      ;;
    --artifacts-root)
      [ "$#" -ge 2 ] || fail 'missing value for --artifacts-root'
      artifacts_root=$2
      shift 2
      ;;
    --package-version)
      [ "$#" -ge 2 ] || fail 'missing value for --package-version'
      package_version=$2
      shift 2
      ;;
    --provenance-identity)
      [ "$#" -ge 2 ] || fail 'missing value for --provenance-identity'
      provenance_identity=$2
      shift 2
      ;;
    --trust-policy)
      [ "$#" -ge 2 ] || fail 'missing value for --trust-policy'
      trust_policy=$2
      shift 2
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

[ -n "$manifest" ] || fail '--manifest is required'
[ -n "$bundle" ] || fail '--bundle is required'
[ -n "$artifacts_root" ] || fail '--artifacts-root is required'
[ -n "$package_version" ] || fail '--package-version is required'
[ -n "$provenance_identity" ] || fail '--provenance-identity is required'
[ -f "$trust_policy" ] || fail "trust policy is missing: $trust_policy"
command -v cosign >/dev/null 2>&1 || fail 'cosign is required'
command -v python3 >/dev/null 2>&1 || fail 'python3 is required'

policy_values=$(python3 - "$trust_policy" "$package_version" "$manifest" <<'PY'
import json
from pathlib import Path
import sys

policy = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
expected = {
    "schema_version",
    "certificate_oidc_issuer",
    "certificate_identity_prefix",
    "required_tag_prefix",
    "source_repository",
    "workflow_trigger",
}
if set(policy) != expected or policy["schema_version"] != 1:
    raise SystemExit("invalid beta trust policy")
manifest = json.loads(Path(sys.argv[3]).read_text(encoding="utf-8"))
provenance = manifest.get("provenance")
if not isinstance(provenance, dict):
    raise SystemExit("manifest provenance is missing")
if provenance.get("source_repository") != policy["source_repository"]:
    raise SystemExit("manifest source repository is not trusted")
source_commit = provenance.get("source_commit")
if not isinstance(source_commit, str):
    raise SystemExit("manifest source commit is missing")
values = (
    policy["certificate_oidc_issuer"],
    policy["certificate_identity_prefix"]
    + policy["required_tag_prefix"]
    + sys.argv[2],
    policy["source_repository"],
    source_commit,
    policy["workflow_trigger"],
)
for value in values:
    if not isinstance(value, str) or not value or "\n" in value:
        raise SystemExit("invalid beta trust policy value")
print(*values, sep="\n")
PY
) || fail 'trust policy validation failed'

issuer=$(printf '%s\n' "$policy_values" | sed -n '1p')
identity=$(printf '%s\n' "$policy_values" | sed -n '2p')
source_repository=$(printf '%s\n' "$policy_values" | sed -n '3p')
source_commit=$(printf '%s\n' "$policy_values" | sed -n '4p')
workflow_trigger=$(printf '%s\n' "$policy_values" | sed -n '5p')
[ -n "$issuer" ] || fail 'trust policy issuer is empty'
[ -n "$identity" ] || fail 'trust policy identity is empty'
[ -n "$source_repository" ] || fail 'trust policy repository is empty'
[ -n "$source_commit" ] || fail 'manifest source commit is empty'
[ -n "$workflow_trigger" ] || fail 'trust policy workflow trigger is empty'

cosign verify-blob \
  --bundle "$bundle" \
  --certificate-identity "$identity" \
  --certificate-oidc-issuer "$issuer" \
  --certificate-github-workflow-repository "$source_repository" \
  --certificate-github-workflow-ref "refs/tags/cli-v${package_version}" \
  --certificate-github-workflow-sha "$source_commit" \
  --certificate-github-workflow-trigger "$workflow_trigger" \
  "$manifest" >/dev/null

"$release_tool" verify \
  --manifest "$manifest" \
  --artifacts-root "$artifacts_root" \
  --expected-package-version "$package_version" \
  --expected-provenance-identity "$provenance_identity"
