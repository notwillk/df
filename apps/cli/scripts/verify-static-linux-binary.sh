#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 <binary> <expected-version>" >&2
}

fail() {
  echo "Static Linux binary verification failed: $*" >&2
  exit 1
}

if [[ $# -ne 2 ]]; then
  usage
  exit 2
fi

binary="$1"
expected_version="$2"

[[ -f $binary ]] || fail "'$binary' is not a regular file"
[[ -x $binary ]] || fail "'$binary' is not executable"
[[ -n $expected_version ]] || fail "expected version must not be empty"
command -v readelf >/dev/null 2>&1 || fail "required command 'readelf' is unavailable"

elf_header="$(LC_ALL=C readelf -h "$binary" 2>&1)" || fail "'$binary' is not an ELF file: $elf_header"
elf_type="$(printf '%s\n' "$elf_header" | sed -n 's/^[[:space:]]*Type:[[:space:]]*//p')"
case "$elf_type" in
  "EXEC (Executable file)" | "DYN (Position-Independent Executable file)") ;;
  *) fail "'$binary' is not an ELF executable (ELF type: ${elf_type:-unknown})" ;;
esac

machine="$(printf '%s\n' "$elf_header" | sed -n 's/^[[:space:]]*Machine:[[:space:]]*//p')"
case "$(uname -m)" in
  x86_64) [[ $machine == "Advanced Micro Devices X86-64" ]] || fail "unexpected ELF machine '$machine'" ;;
  aarch64 | arm64) [[ $machine == AArch64 ]] || fail "unexpected ELF machine '$machine'" ;;
  *) fail "unsupported verification host architecture '$(uname -m)'" ;;
esac

program_headers="$(LC_ALL=C readelf -lW "$binary" 2>&1)" || fail "unable to inspect program headers: $program_headers"
if grep -Eq '(^|[[:space:]])INTERP([[:space:]]|$)|Requesting program interpreter' <<<"$program_headers"; then
  fail "'$binary' has a program interpreter"
fi

dynamic_section="$(LC_ALL=C readelf -dW "$binary" 2>&1)" || fail "unable to inspect dynamic section: $dynamic_section"
if grep -Eq '\(NEEDED\)' <<<"$dynamic_section"; then
  fail "'$binary' has dynamic NEEDED entries"
fi

version_info="$(LC_ALL=C readelf --version-info "$binary" 2>&1)" || fail "unable to inspect version requirements: $version_info"
if grep -Eq 'GLIBC_' <<<"$version_info"; then
  fail "'$binary' has GLIBC version requirements"
fi

reported_version="$("$binary" --version)" || fail "'$binary --version' exited unsuccessfully"
[[ $reported_version == "dof $expected_version" ]] || fail "reported '$reported_version', expected 'dof $expected_version'"
"$binary" --help >/dev/null || fail "'$binary --help' exited unsuccessfully"

lint_workspace="$(mktemp -d)"
trap 'rm -rf -- "$lint_workspace"' EXIT
"$binary" lint "$lint_workspace" || fail "'$binary lint' exited unsuccessfully"

printf 'Verified static Linux binary: %s (dof %s)\n' "$binary" "$expected_version"
