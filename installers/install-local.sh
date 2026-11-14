#!/bin/sh

set -eu

fail() {
  printf 'install-local: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: installers/install-local.sh <source>

<source> is one of:
  - a local directory containing one matching tarball and SHA256SUMS
  - a local compresso-<version>-<target>.tar.gz path
  - an https:// URL to an immutable versioned tarball
EOF
}

detect_target() {
  system=$(uname -s)
  machine=$(uname -m)
  case "$system:$machine" in
    Darwin:arm64 | Darwin:aarch64) printf '%s\n' aarch64-apple-darwin ;;
    Darwin:x86_64 | Darwin:amd64) printf '%s\n' x86_64-apple-darwin ;;
    Linux:x86_64 | Linux:amd64) printf '%s\n' x86_64-unknown-linux-gnu ;;
    *) fail "unsupported platform: $system $machine" ;;
  esac
}

sha256_file() {
  input=$1
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$input" | awk '{ print $1 }'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$input" | awk '{ print $1 }'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$input" | awk '{ print $NF }'
  else
    fail "no SHA-256 implementation found (need shasum, sha256sum, or openssl)"
  fi
}

download() {
  url=$1
  output=$2
  if command -v curl >/dev/null 2>&1; then
    # Redirects may not leave HTTPS either.
    curl --fail --silent --show-error --location \
      --proto '=https' --proto-redir '=https' --output "$output" "$url" ||
      fail "download failed: $url"
  elif command -v wget >/dev/null 2>&1; then
    wget -q --https-only -O "$output" "$url" || fail "download failed: $url"
  else
    fail "URL source requires curl or wget"
  fi
}

[ "$#" -eq 1 ] || {
  usage >&2
  exit 2
}
[ -n "${HOME:-}" ] || fail "HOME is not set"

source=$1
target=$(detect_target)
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/compresso-install.XXXXXX")
install_tmp=
backup_tmp=

cleanup() {
  [ -z "$install_tmp" ] || rm -f "$install_tmp"
  [ -z "$backup_tmp" ] || rm -f "$backup_tmp"
  rm -rf "$work_dir"
}
trap cleanup 0 1 2 3 15

case "$source" in
  http://*)
    fail "plain HTTP sources are not accepted; use an https:// URL or a local path"
    ;;
  https://*)
    case "$source" in
      *\?* | *\#*) fail "artifact URL must not contain a query or fragment" ;;
    esac
    archive_name=${source##*/}
    source_base=${source%/*}
    case "$archive_name" in
      compresso-*-$target.tar.gz) ;;
      *) fail "URL does not name a $target release tarball: $archive_name" ;;
    esac
    download "$source" "$work_dir/$archive_name"
    download "$source_base/SHA256SUMS" "$work_dir/SHA256SUMS"
    ;;
  *)
    if [ -d "$source" ]; then
      source_dir=$(CDPATH='' cd -P "$source" && pwd)
      set -- "$source_dir"/compresso-*-"$target".tar.gz
      [ "$#" -eq 1 ] && [ -f "$1" ] ||
        fail "local source must contain exactly one $target release tarball"
      archive_source=$1
    elif [ -f "$source" ]; then
      source_dir=$(CDPATH='' cd -P "$(dirname "$source")" && pwd)
      archive_source="$source_dir/$(basename "$source")"
    else
      fail "source is not a local path or HTTP(S) tarball URL: $source"
    fi

    archive_name=$(basename "$archive_source")
    case "$archive_name" in
      compresso-*-$target.tar.gz) ;;
      *) fail "local artifact is not a $target release tarball: $archive_name" ;;
    esac
    [ -f "$source_dir/SHA256SUMS" ] || fail "missing checksum manifest: $source_dir/SHA256SUMS"
    cp "$archive_source" "$work_dir/$archive_name"
    cp "$source_dir/SHA256SUMS" "$work_dir/SHA256SUMS"
    ;;
esac

case "$archive_name" in
  '' | *[!A-Za-z0-9._+-]*) fail "invalid release tarball filename: $archive_name" ;;
esac

expected=$(
  awk -v name="$archive_name" '
    $2 == name {
      if (found) exit 3
      expected = $1
      found = 1
    }
    END {
      if (!found) exit 2
      print expected
    }
  ' "$work_dir/SHA256SUMS"
) || fail "checksum manifest must contain exactly one entry for $archive_name"

[ "${#expected}" -eq 64 ] || fail "invalid expected checksum for $archive_name"
case "$expected" in
  *[!0-9A-Fa-f]*) fail "invalid expected checksum for $archive_name" ;;
esac

actual=$(sha256_file "$work_dir/$archive_name")
expected_normalized=$(printf '%s' "$expected" | tr 'A-F' 'a-f')
actual_normalized=$(printf '%s' "$actual" | tr 'A-F' 'a-f')
if [ "$actual_normalized" != "$expected_normalized" ]; then
  fail "checksum mismatch for $archive_name: expected $expected_normalized, actual $actual_normalized"
fi

payload_listing=$(tar -tzf "$work_dir/$archive_name") || fail "cannot read verified tarball"
[ "$payload_listing" = compresso ] || fail "verified tarball must contain exactly one compresso file"
mkdir "$work_dir/extract"
tar -xzf "$work_dir/$archive_name" -C "$work_dir/extract" compresso ||
  fail "cannot unpack verified tarball"
payload="$work_dir/extract/compresso"
[ -f "$payload" ] && [ ! -L "$payload" ] || fail "tarball payload must be a regular file"

install_dir="$HOME/.local/bin"
destination="$install_dir/compresso"
previous="$install_dir/compresso.previous"
mkdir -p "$install_dir"

install_tmp=$(mktemp "$install_dir/.compresso.install.XXXXXX")
cp "$payload" "$install_tmp"
chmod 0755 "$install_tmp"

if [ -e "$destination" ] || [ -L "$destination" ]; then
  [ -f "$destination" ] && [ ! -L "$destination" ] ||
    fail "existing install is not a regular file: $destination"
  backup_tmp=$(mktemp "$install_dir/.compresso.previous.XXXXXX")
  cp "$destination" "$backup_tmp"
  chmod 0755 "$backup_tmp"
  mv -f "$backup_tmp" "$previous"
  backup_tmp=
fi

mv -f "$install_tmp" "$destination"
install_tmp=
printf 'Installed %s at %s\n' "$archive_name" "$destination"

case ":${PATH:-}:" in
  *":$install_dir:"*) ;;
  *)
    printf '%s\n' "Add $install_dir to PATH, for example:"
    # This is literal guidance for the user's shell startup file.
    # shellcheck disable=SC2016
    printf '%s\n' '  export PATH="$HOME/.local/bin:$PATH"'
    ;;
esac
