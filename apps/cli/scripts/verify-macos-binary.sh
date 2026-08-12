#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 <binary> <expected-version>" >&2
}

fail() {
  echo "macOS binary verification failed: $*" >&2
  exit 1
}

if [[ $# -ne 2 ]]; then
  usage
  exit 2
fi

binary="$1"
expected_version="$2"

[[ $(uname -s) == Darwin ]] || fail "verification must run on macOS"
[[ $(uname -m) == arm64 ]] || fail "verification must run on Apple Silicon"
[[ -f $binary ]] || fail "'$binary' is not a regular file"
[[ -x $binary ]] || fail "'$binary' is not executable"
[[ -n $expected_version ]] || fail "expected version must not be empty"
command -v file >/dev/null 2>&1 || fail "required command 'file' is unavailable"
command -v lipo >/dev/null 2>&1 || fail "required command 'lipo' is unavailable"

file_description="$(LC_ALL=C file "$binary")" || fail "unable to inspect '$binary'"
[[ $file_description == *"Mach-O 64-bit executable arm64"* ]] || fail "unexpected binary format: $file_description"
architectures="$(lipo -archs "$binary")" || fail "unable to inspect architectures for '$binary'"
[[ $architectures == arm64 ]] || fail "expected only arm64, found '$architectures'"

reported_version="$("$binary" --version)" || fail "'$binary --version' exited unsuccessfully"
[[ $reported_version == "dof $expected_version" ]] || fail "reported '$reported_version', expected 'dof $expected_version'"
"$binary" --help >/dev/null || fail "'$binary --help' exited unsuccessfully"

lint_workspace="$(mktemp -d)"
trap 'rm -rf -- "$lint_workspace"' EXIT
"$binary" lint "$lint_workspace" || fail "'$binary lint' exited unsuccessfully"

printf 'Verified macOS arm64 binary: %s (dof %s)\n' "$binary" "$expected_version"
