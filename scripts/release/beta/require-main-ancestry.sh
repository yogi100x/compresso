#!/bin/sh

# Refuses to package a commit that never reached the main branch: a release
# tag pushed onto an unreviewed side branch must not produce signed artifacts.

set -eu

fail() {
  printf 'require-main-ancestry: %s\n' "$*" >&2
  exit 1
}

[ "$#" -eq 2 ] || fail 'usage: require-main-ancestry.sh <commit> <main-reference>'
commit=$1
reference=$2

git rev-parse --verify --quiet "${commit}^{commit}" >/dev/null ||
  fail "cannot resolve release commit: $commit"
git rev-parse --verify --quiet "${reference}^{commit}" >/dev/null ||
  fail "cannot resolve main reference: $reference"
git merge-base --is-ancestor "$commit" "$reference" ||
  fail "release commit $commit is not on $reference"
