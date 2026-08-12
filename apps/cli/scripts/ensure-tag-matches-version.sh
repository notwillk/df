#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 <vX.Y.Z>" >&2
  exit 2
fi

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
version="$("$script_dir/get-version.sh")"
expected_tag="v$version"

if [[ $1 != "$expected_tag" ]]; then
  echo "Release tag '$1' does not match workspace version '$expected_tag'" >&2
  exit 1
fi

printf '%s\n' "$version"
