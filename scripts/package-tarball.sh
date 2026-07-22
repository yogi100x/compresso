#!/bin/sh

set -eu

FIXED_MTIME=946684800

fail() {
  printf 'package-tarball: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage:
  scripts/package-tarball.sh <binary> <version> <target> <output-dir>
  scripts/package-tarball.sh --checksums <output-file> <tarball>...
  scripts/package-tarball.sh --self-test
EOF
}

script_dir=$(CDPATH='' cd -P "$(dirname "$0")" && pwd)
script_path="$script_dir/$(basename "$0")"

sha256_file() {
  checksum_file=$1
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$checksum_file" | awk '{ print $1 }'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$checksum_file" | awk '{ print $1 }'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$checksum_file" | awk '{ print $NF }'
  else
    fail "no SHA-256 implementation found (need shasum, sha256sum, or openssl)"
  fi
}

verify_checksums() (
  manifest=$1
  manifest_dir=$(CDPATH='' cd -P "$(dirname "$manifest")" && pwd)
  manifest_name=$(basename "$manifest")

  if command -v shasum >/dev/null 2>&1; then
    (cd "$manifest_dir" && shasum -a 256 -c "$manifest_name")
  elif command -v sha256sum >/dev/null 2>&1; then
    (cd "$manifest_dir" && sha256sum -c "$manifest_name")
  else
    while read -r expected_hash filename; do
      [ -n "$expected_hash" ] || continue
      [ "$(sha256_file "$manifest_dir/$filename")" = "$expected_hash" ] || exit 1
    done < "$manifest"
  fi
)

create_package() (
  binary=$1
  version=$2
  target=$3
  output_dir=$4

  [ -f "$binary" ] || fail "binary does not exist: $binary"
  case "$version" in
    '' | *[!A-Za-z0-9.+-]*) fail "invalid version: $version" ;;
  esac

  case "$target" in
    aarch64-apple-darwin | x86_64-apple-darwin | x86_64-unknown-linux-gnu)
      entry_name=compresso
      ;;
    x86_64-pc-windows-msvc)
      entry_name=compresso.exe
      ;;
    *)
      fail "unsupported target: $target"
      ;;
  esac

  binary_dir=$(CDPATH='' cd -P "$(dirname "$binary")" && pwd)
  binary_path="$binary_dir/$(basename "$binary")"
  mkdir -p "$output_dir"
  output_dir=$(CDPATH='' cd -P "$output_dir" && pwd)

  archive_name="compresso-$version-$target.tar.gz"
  archive_path="$output_dir/$archive_name"
  archive_tmp="$output_dir/.$archive_name.tmp.$$"
  stage_dir=$(mktemp -d)
  tar_path="$stage_dir/payload.tar"

  # Invoked through trap.
  # shellcheck disable=SC2329
  cleanup_package() {
    rm -f "$archive_tmp" "$tar_path" "$stage_dir/$entry_name"
    rmdir "$stage_dir" 2>/dev/null || true
  }
  trap cleanup_package 0 1 2 3 15

  cp "$binary_path" "$stage_dir/$entry_name"
  chmod 0755 "$stage_dir/$entry_name"
  TZ=UTC0 touch -t 200001010000.00 "$stage_dir/$entry_name"

  if tar --version 2>/dev/null | grep -q 'GNU tar'; then
    (
      cd "$stage_dir"
      COPYFILE_DISABLE=1 tar \
        --format=ustar \
        --sort=name \
        --mtime="@$FIXED_MTIME" \
        --owner=0 \
        --group=0 \
        --numeric-owner \
        -cf "$tar_path" \
        "$entry_name"
    )
  else
    (
      cd "$stage_dir"
      COPYFILE_DISABLE=1 tar \
        --format=ustar \
        --uid 0 \
        --gid 0 \
        --uname root \
        --gname root \
        -cf "$tar_path" \
        "$entry_name"
    )
  fi

  gzip -n -9 -c "$tar_path" > "$archive_tmp"
  mv -f "$archive_tmp" "$archive_path"
  printf '%s\n' "$archive_path"
)

create_checksums() (
  output=$1
  shift
  [ "$#" -gt 0 ] || fail "--checksums requires at least one tarball"

  mkdir -p "$(dirname "$output")"
  output_dir=$(CDPATH='' cd -P "$(dirname "$output")" && pwd)
  output_name=$(basename "$output")
  case "$output_name" in
    '' | *[!A-Za-z0-9._+-]*) fail "invalid checksum filename: $output_name" ;;
  esac
  output="$output_dir/$output_name"
  checksum_tmp=$(mktemp "$output_dir/.SHA256SUMS.unsorted.XXXXXX")
  sorted_tmp=$(mktemp "$output_dir/.SHA256SUMS.sorted.XXXXXX")

  # Invoked through trap.
  # shellcheck disable=SC2329
  cleanup_checksums() {
    rm -f "$checksum_tmp" "$sorted_tmp"
  }
  trap cleanup_checksums 0 1 2 3 15

  seen_names=' '
  for archive in "$@"; do
    [ -f "$archive" ] || fail "tarball does not exist: $archive"
    archive_dir=$(CDPATH='' cd -P "$(dirname "$archive")" && pwd)
    archive_name=$(basename "$archive")
    case "$archive_name" in
      '' | *[!A-Za-z0-9._+-]*) fail "invalid tarball filename: $archive_name" ;;
    esac
    case "$seen_names" in
      *" $archive_name "*) fail "duplicate tarball filename: $archive_name" ;;
    esac
    seen_names="$seen_names$archive_name "
    archive_hash=$(sha256_file "$archive_dir/$archive_name")
    printf '%s  %s\n' "$archive_hash" "$archive_name" >> "$checksum_tmp"
  done

  LC_ALL=C sort -k2,2 "$checksum_tmp" > "$sorted_tmp"
  mv -f "$sorted_tmp" "$output"
  printf '%s\n' "$output"
)

self_test() (
  test_root=$(mktemp -d)
  first_dir="$test_root/first"
  second_dir="$test_root/second"
  windows_dir="$test_root/windows"
  stub_binary="$test_root/stub-binary"
  archive_name='compresso-0.0.0-test-x86_64-unknown-linux-gnu.tar.gz'
  windows_archive_name='compresso-0.0.0-test-x86_64-pc-windows-msvc.tar.gz'
  mkdir -p "$first_dir" "$second_dir" "$windows_dir"

  # Invoked through trap.
  # shellcheck disable=SC2329
  cleanup_self_test() {
    rm -f \
      "$stub_binary" \
      "$first_dir/$archive_name" \
      "$first_dir/SHA256SUMS" \
      "$second_dir/$archive_name" \
      "$windows_dir/$windows_archive_name"
    rmdir "$first_dir" "$second_dir" "$windows_dir" "$test_root" 2>/dev/null || true
  }
  trap cleanup_self_test 0 1 2 3 15

  printf '#!/bin/sh\nprintf "compresso test binary\\n"\n' > "$stub_binary"
  chmod 0755 "$stub_binary"

  "$script_path" "$stub_binary" 0.0.0-test x86_64-unknown-linux-gnu "$first_dir" >/dev/null
  "$script_path" "$stub_binary" 0.0.0-test x86_64-unknown-linux-gnu "$second_dir" >/dev/null

  archive_listing=$(tar -tzf "$first_dir/$archive_name")
  [ "$archive_listing" = 'compresso' ] || fail "archive must contain exactly compresso"
  cmp -s "$first_dir/$archive_name" "$second_dir/$archive_name" || \
    fail "identical inputs produced different archives"

  "$script_path" "$stub_binary" 0.0.0-test x86_64-pc-windows-msvc "$windows_dir" >/dev/null
  windows_listing=$(tar -tzf "$windows_dir/$windows_archive_name")
  [ "$windows_listing" = 'compresso.exe' ] || \
    fail "Windows archive must contain exactly compresso.exe"

  "$script_path" --checksums "$first_dir/SHA256SUMS" "$first_dir/$archive_name" >/dev/null
  verify_checksums "$first_dir/SHA256SUMS" >/dev/null
  printf 'tampered' >> "$first_dir/$archive_name"
  if verify_checksums "$first_dir/SHA256SUMS" >/dev/null 2>&1; then
    fail "checksum verification accepted a tampered archive"
  fi

  printf 'package-tarball self-test: ok\n'
)

case "${1:-}" in
  --checksums)
    shift
    [ "$#" -ge 2 ] || {
      usage >&2
      exit 2
    }
    create_checksums "$@"
    ;;
  --self-test)
    shift
    [ "$#" -eq 0 ] || {
      usage >&2
      exit 2
    }
    self_test
    ;;
  -h | --help)
    usage
    ;;
  *)
    [ "$#" -eq 4 ] || {
      usage >&2
      exit 2
    }
    create_package "$@"
    ;;
esac
