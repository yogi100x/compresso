#!/bin/sh

set -eu

test_dir=$(CDPATH='' cd -P "$(dirname "$0")" && pwd)
repo_root=$(CDPATH='' cd -P "$test_dir/../.." && pwd)
installer="$repo_root/installers/install-local.sh"
packager="$repo_root/scripts/package-tarball.sh"
test_root=$(mktemp -d)

cleanup() {
  rm -rf "$test_root"
}
trap cleanup 0 1 2 3 15

fail() {
  printf 'install_local_test: %s\n' "$*" >&2
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

host_target() {
  system=$(uname -s)
  machine=$(uname -m)
  case "$system:$machine" in
    Darwin:arm64 | Darwin:aarch64) printf '%s\n' aarch64-apple-darwin ;;
    Darwin:x86_64 | Darwin:amd64) printf '%s\n' x86_64-apple-darwin ;;
    Linux:x86_64 | Linux:amd64) printf '%s\n' x86_64-unknown-linux-gnu ;;
    *) fail "test host is unsupported: $system $machine" ;;
  esac
}

file_mode() {
  if stat -f '%Lp' "$1" >/dev/null 2>&1; then
    stat -f '%Lp' "$1"
  else
    stat -c '%a' "$1"
  fi
}

create_release() {
  release_dir=$1
  version=$2
  marker=$3
  target=$(host_target)
  stub="$release_dir/stub-$version"
  mkdir -p "$release_dir"
  cat > "$stub" <<EOF
#!/bin/sh
case "\${1:-}" in
  --version) printf '%s\n' 'compresso $version' ;;
  doctor) printf '%s\n' 'doctor $marker: ok' ;;
  *) printf '%s\n' '$marker' ;;
esac
EOF
  chmod 0755 "$stub"
  "$packager" "$stub" "$version" "$target" "$release_dir" >/dev/null
  archive="$release_dir/compresso-$version-$target.tar.gz"
  "$packager" --checksums "$release_dir/SHA256SUMS" "$archive" >/dev/null
  printf '%s\n' "$archive"
}

test_install_clean_home() {
  case_root="$test_root/clean"
  home="$case_root/home"
  release="$case_root/release"
  mkdir -p "$home"
  create_release "$release" 1.2.3 clean >/dev/null

  output=$(HOME="$home" PATH="/usr/bin:/bin" "$installer" "$release")
  binary="$home/.local/bin/compresso"

  [ -x "$binary" ] || fail "clean install did not create an executable"
  [ "$(file_mode "$binary")" = 755 ] || fail "installed mode is not 0755"
  [ "$($binary --version)" = 'compresso 1.2.3' ] || fail "installed version mismatch"
  "$binary" doctor >/dev/null || fail "installed doctor command failed"
  assert_contains "$output" 'PATH'
}

test_install_from_direct_local_tarball() {
  case_root="$test_root/direct"
  home="$case_root/home"
  release="$case_root/release"
  mkdir -p "$home"
  archive=$(create_release "$release" 1.3.0 direct)

  HOME="$home" "$installer" "$archive" >/dev/null

  [ "$("$home/.local/bin/compresso" --version)" = 'compresso 1.3.0' ] ||
    fail "direct local tarball install version mismatch"
}

test_checksum_mismatch_aborts_before_install_write() {
  case_root="$test_root/checksum"
  home="$case_root/home"
  release="$case_root/release"
  mkdir -p "$home/.local/bin"
  printf '%s\n' untouched > "$home/.local/bin/sentinel"
  archive=$(create_release "$release" 2.0.0 checksum)
  printf 'tampered' >> "$archive"

  if HOME="$home" "$installer" "$release" >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "checksum mismatch unexpectedly installed"
  fi

  error=$(cat "$case_root/stderr")
  assert_contains "$error" 'checksum'
  assert_contains "$error" 'expected'
  assert_contains "$error" 'actual'
  [ "$(cat "$home/.local/bin/sentinel")" = untouched ] || fail "sentinel changed"
  [ "$(find "$home/.local/bin" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ')" = 1 ] ||
    fail "checksum failure wrote under the install directory"
}

test_install_idempotent_with_previous_backup() {
  case_root="$test_root/idempotent"
  home="$case_root/home"
  first="$case_root/first"
  second="$case_root/second"
  mkdir -p "$home"
  create_release "$first" 3.0.0 first >/dev/null
  create_release "$second" 3.1.0 second >/dev/null

  HOME="$home" "$installer" "$first" >/dev/null
  HOME="$home" "$installer" "$second" >/dev/null

  binary="$home/.local/bin/compresso"
  previous="$home/.local/bin/compresso.previous"
  [ "$($binary --version)" = 'compresso 3.1.0' ] || fail "replacement is not current"
  [ "$($previous --version)" = 'compresso 3.0.0' ] || fail "previous backup is not prior binary"
  [ "$(file_mode "$binary")" = 755 ] || fail "replacement mode is not 0755"
  [ "$(file_mode "$previous")" = 755 ] || fail "backup mode is not 0755"
}

test_multiple_payload_archive_rejected() {
  case_root="$test_root/multiple"
  home="$case_root/home"
  release="$case_root/release"
  stage="$case_root/stage"
  target=$(host_target)
  archive="$release/compresso-3.2.0-$target.tar.gz"
  mkdir -p "$home" "$release" "$stage"
  printf '%s\n' '#!/bin/sh' 'printf "%s\\n" compresso' > "$stage/compresso"
  printf '%s\n' unexpected > "$stage/extra"
  chmod 0755 "$stage/compresso"
  tar -czf "$archive" -C "$stage" compresso extra
  "$packager" --checksums "$release/SHA256SUMS" "$archive" >/dev/null

  if HOME="$home" "$installer" "$release" >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "multiple-payload archive unexpectedly installed"
  fi

  assert_contains "$(cat "$case_root/stderr")" 'exactly one compresso file'
  [ ! -e "$home/.local/bin" ] || fail "invalid payload wrote under install directory"
}

test_no_config_or_assistant_writes() {
  case_root="$test_root/config"
  home="$case_root/home"
  release="$case_root/release"
  mkdir -p "$home/.config/compresso" "$home/.claude" "$home/.codex"
  printf '%s\n' 'mode = "safe"' > "$home/.config/compresso/config.toml"
  printf '%s\n' '{"sentinel":"claude"}' > "$home/.claude/settings.json"
  printf '%s\n' 'sentinel = "codex"' > "$home/.codex/config.toml"
  before=$(find "$home/.config" "$home/.claude" "$home/.codex" -type f -exec shasum -a 256 {} \; | LC_ALL=C sort)
  create_release "$release" 4.0.0 config >/dev/null

  HOME="$home" "$installer" "$release" >/dev/null

  after=$(find "$home/.config" "$home/.claude" "$home/.codex" -type f -exec shasum -a 256 {} \; | LC_ALL=C sort)
  [ "$after" = "$before" ] || fail "installer mutated configuration"
}

test_install_from_versioned_url() {
  case_root="$test_root/url"
  home="$case_root/home"
  release="$case_root/release"
  fake_bin="$case_root/bin"
  mkdir -p "$home" "$fake_bin"
  archive=$(create_release "$release" 5.0.0 url)
  archive_name=$(basename "$archive")
  cat > "$fake_bin/curl" <<'EOF'
#!/bin/sh
output=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output|-o)
      output=$2
      shift 2
      ;;
    -* ) shift ;;
    * ) url=$1; shift ;;
  esac
done
[ -n "$output" ] && [ -n "$url" ] || exit 2
cp "$FIXTURE_SOURCE/$(basename "$url")" "$output"
EOF
  chmod 0755 "$fake_bin/curl"

  HOME="$home" FIXTURE_SOURCE="$release" PATH="$fake_bin:/usr/bin:/bin" \
    "$installer" "https://fixtures.invalid/releases/$archive_name" >/dev/null

  [ "$("$home/.local/bin/compresso" --version)" = 'compresso 5.0.0' ] ||
    fail "URL install version mismatch"
}

test_plain_http_url_rejected_before_any_download() {
  home="$test_root/http-home"
  fake_bin="$test_root/http-bin"
  mkdir -p "$home" "$fake_bin"
  target=$(host_target)
  cat > "$fake_bin/curl" <<'EOF'
#!/bin/sh
: > "$DOWNLOAD_MARKER"
exit 1
EOF
  chmod 0755 "$fake_bin/curl"
  cp "$fake_bin/curl" "$fake_bin/wget"
  marker="$test_root/http-download-attempted"

  if HOME="$home" DOWNLOAD_MARKER="$marker" PATH="$fake_bin:/usr/bin:/bin" \
    "$installer" "http://fixtures.invalid/releases/compresso-5.0.0-$target.tar.gz" \
    > "$test_root/http.out" 2> "$test_root/http.err"; then
    fail "plain HTTP URL was accepted"
  fi
  assert_contains "$(cat "$test_root/http.err")" 'https://'
  [ ! -e "$marker" ] || fail "plain HTTP URL reached the downloader"
  [ ! -e "$home/.local/bin/compresso" ] || fail "plain HTTP URL installed a binary"
}

test_url_download_is_restricted_to_https() {
  home="$test_root/proto-home"
  release="$test_root/proto-release"
  fake_bin="$test_root/proto-bin"
  mkdir -p "$home" "$fake_bin"
  create_release "$release" 5.0.0 proto >/dev/null
  target=$(host_target)
  cat > "$fake_bin/curl" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$CURL_ARGUMENTS"
output=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output|-o) output=$2; shift 2 ;;
    --proto|--proto-redir) shift 2 ;;
    -* ) shift ;;
    * ) url=$1; shift ;;
  esac
done
[ -n "$output" ] && [ -n "$url" ] || exit 2
cp "$FIXTURE_SOURCE/$(basename "$url")" "$output"
EOF
  chmod 0755 "$fake_bin/curl"
  arguments="$test_root/proto-curl-arguments"

  HOME="$home" FIXTURE_SOURCE="$release" CURL_ARGUMENTS="$arguments" \
    PATH="$fake_bin:/usr/bin:/bin" \
    "$installer" "https://fixtures.invalid/releases/compresso-5.0.0-$target.tar.gz" >/dev/null

  assert_contains "$(cat "$arguments")" '--proto =https'
  assert_contains "$(cat "$arguments")" '--proto-redir =https'
}

test_wget_fallback_never_follows_redirects() {
  home="$test_root/wget-home"
  release="$test_root/wget-release"
  tools="$test_root/wget-tools"
  mkdir -p "$home" "$tools"
  create_release "$release" 5.0.0 wget >/dev/null
  target=$(host_target)
  # Every system tool except curl, so the installer takes its wget path.
  for tool in /usr/bin/* /bin/*; do
    name=${tool##*/}
    [ "$name" = curl ] || [ -e "$tools/$name" ] || ln -s "$tool" "$tools/$name"
  done
  rm -f "$tools/wget"
  cat > "$tools/wget" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$WGET_ARGUMENTS"
output=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -O) output=$2; shift 2 ;;
    -* ) shift ;;
    * ) url=$1; shift ;;
  esac
done
[ -n "$output" ] && [ -n "$url" ] || exit 2
cp "$FIXTURE_SOURCE/$(basename "$url")" "$output"
EOF
  chmod 0755 "$tools/wget"
  arguments="$test_root/wget-arguments"

  HOME="$home" FIXTURE_SOURCE="$release" WGET_ARGUMENTS="$arguments" PATH="$tools" \
    "$installer" "https://fixtures.invalid/releases/compresso-5.0.0-$target.tar.gz" >/dev/null

  assert_contains "$(cat "$arguments")" '--max-redirect=0'
}

test_install_clean_home
test_install_from_direct_local_tarball
test_checksum_mismatch_aborts_before_install_write
test_install_idempotent_with_previous_backup
test_multiple_payload_archive_rejected
test_no_config_or_assistant_writes
test_install_from_versioned_url
test_plain_http_url_rejected_before_any_download
test_url_download_is_restricted_to_https
test_wget_fallback_never_follows_redirects
printf '%s\n' 'install_local_test: all tests passed'
