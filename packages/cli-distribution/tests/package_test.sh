#!/bin/sh

set -eu

test_dir=$(CDPATH='' cd -P "$(dirname "$0")" && pwd)
package_root=$(CDPATH='' cd -P "$test_dir/.." && pwd)
test_root=$(mktemp -d)

cleanup() {
  rm -rf "$test_root"
}
trap cleanup 0 1 2 3 15

fail() {
  printf 'cli_distribution_test: %s\n' "$*" >&2
  exit 1
}

assert_contains() {
  haystack=$1
  needle=$2
  case "$haystack" in
    *"$needle"*) ;;
    *) fail "expected output to contain: $needle" ;;
  esac
}

sha256_file() {
  subject=$1
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$subject" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$subject" | awk '{ print $1 }'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$subject" | awk '{ print $NF }'
  else
    fail 'test host has no SHA-256 implementation'
  fi
}

host_target() {
  system=$(uname -s)
  machine=$(uname -m)
  case "$system:$machine" in
    Darwin:arm64 | Darwin:aarch64) printf '%s\n' darwin-arm64 ;;
    Darwin:x86_64 | Darwin:amd64) printf '%s\n' darwin-x64 ;;
    Linux:x86_64 | Linux:amd64) printf '%s\n' linux-x64 ;;
    *) fail "test host is unsupported: $system $machine" ;;
  esac
}

write_fake_artifact() {
  fixture=$1
  target=$2
  artifact_dir="$fixture/vendor/$target"
  artifact="$artifact_dir/compresso"
  mkdir -p "$artifact_dir"
  cat > "$artifact" <<EOF
#!/bin/sh
printf 'artifact-target=%s\\n' '$target'
printf 'launcher-path=%s\\n' "\${COMPRESSO_LAUNCHER_PATH:-}"
for argument in "\$@"; do
  printf 'argument=%s\\n' "\$argument"
done
EOF
  chmod 0755 "$artifact"
  digest=$(sha256_file "$artifact")
  cat > "$artifact_dir/artifact.manifest" <<EOF
schema-version: 1
target: $target
binary: compresso
sha256: $digest
EOF
}

run_expect_failure() {
  expected=$1
  shift
  stdout_file="$test_root/failure.stdout"
  stderr_file="$test_root/failure.stderr"
  if "$@" >"$stdout_file" 2>"$stderr_file"; then
    fail "command unexpectedly succeeded: $*"
  fi
  assert_contains "$(cat "$stderr_file")" "$expected"
}

fixture="$test_root/package fixture"
pack_dir="$test_root/packed"
unpack_dir="$test_root/unpacked"
mkdir -p "$fixture" "$pack_dir" "$unpack_dir"
cp -R "$package_root/." "$fixture/"

if grep -E '"(install|postinstall|prepare|dependencies|optionalDependencies|peerDependencies)"[[:space:]]*:' \
  "$fixture/package.json" >/dev/null; then
  fail 'package metadata contains a lifecycle hook or runtime dependency block'
fi
[ "$(sed -n '1p' "$fixture/bin/compresso")" = '#!/bin/sh' ] ||
  fail 'launcher must use the POSIX shell instead of a Node.js runtime'

write_fake_artifact "$fixture" darwin-arm64
write_fake_artifact "$fixture" darwin-x64
write_fake_artifact "$fixture" linux-x64

package_archive=$(cd "$fixture" && npm pack --silent --pack-destination "$pack_dir")
archive="$pack_dir/$package_archive"
[ -f "$archive" ] || fail "npm pack did not create $archive"

actual_listing="$test_root/actual-listing"
expected_listing="$test_root/expected-listing"
tar -tzf "$archive" | LC_ALL=C sort > "$actual_listing"
cat > "$expected_listing" <<'EOF'
package/LICENSE
package/README.md
package/THIRD_PARTY_NOTICES.md
package/bin/compresso
package/package.json
package/vendor/darwin-arm64/artifact.manifest
package/vendor/darwin-arm64/compresso
package/vendor/darwin-x64/artifact.manifest
package/vendor/darwin-x64/compresso
package/vendor/linux-x64/artifact.manifest
package/vendor/linux-x64/compresso
EOF
LC_ALL=C sort -o "$expected_listing" "$expected_listing"
cmp -s "$actual_listing" "$expected_listing" || {
  diff -u "$expected_listing" "$actual_listing" >&2 || true
  fail 'npm package contents differ from the distribution allowlist'
}

if tar -tzf "$archive" | grep -E '(tests?/|fixtures?/|src/|\.map$|\.dSYM/|\.pdb$|\.debug$)' >/dev/null; then
  fail 'npm package contains source, test, fixture, map, or symbol files'
fi

tar -xzf "$archive" -C "$unpack_dir"
launcher="$unpack_dir/package/bin/compresso"
[ -x "$launcher" ] || fail 'packed launcher is not executable'

target=$(host_target)
direct_output=$("$launcher" 'direct argument')
assert_contains "$direct_output" "artifact-target=$target"
assert_contains "$direct_output" "launcher-path=$launcher"
assert_contains "$direct_output" 'argument=direct argument'

global_bin="$test_root/global bin"
first_link_dir="$test_root/pnpm store/bin"
mkdir -p "$global_bin" "$first_link_dir"
ln -s "$launcher" "$first_link_dir/compresso"
ln -s "$first_link_dir/compresso" "$global_bin/compresso"

symlink_output=$(PATH="$global_bin:$PATH" compresso 'symlink argument')
assert_contains "$symlink_output" "artifact-target=$target"
assert_contains "$symlink_output" "launcher-path=$global_bin/compresso"
assert_contains "$symlink_output" 'argument=symlink argument'

unsupported_bin="$test_root/unsupported-bin"
mkdir -p "$unsupported_bin"
cat > "$unsupported_bin/uname" <<'EOF'
#!/bin/sh
case "${1:-}" in
  -s) printf '%s\n' Plan9 ;;
  -m) printf '%s\n' mips64 ;;
  *) exit 2 ;;
esac
EOF
chmod 0755 "$unsupported_bin/uname"
run_expect_failure 'unsupported platform: Plan9 mips64' \
  env PATH="$unsupported_bin:$PATH" "$launcher"

artifact="$unpack_dir/package/vendor/$target/compresso"
saved_artifact="$test_root/saved-artifact"
cp "$artifact" "$saved_artifact"
rm "$artifact"
run_expect_failure "native binary is missing for $target" "$launcher"
cp "$saved_artifact" "$artifact"
chmod 0755 "$artifact"
printf '%s\n' tampered >> "$artifact"
run_expect_failure "native binary failed integrity verification for $target" "$launcher"

printf '%s\n' 'cli_distribution_test: all tests passed'
