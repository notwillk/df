#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
workspace_root="$(CDPATH= cd -- "$script_dir/../../.." && pwd)"
manifest="$workspace_root/Cargo.toml"

version="$("${AWK:-awk}" '
  /^\[workspace\.package\][[:space:]]*$/ { in_package = 1; next }
  /^\[/ { in_package = 0 }
  in_package && /^[[:space:]]*version[[:space:]]*=/ {
    value = $0
    sub(/^[^=]*=[[:space:]]*"/, "", value)
    sub(/"[[:space:]]*$/, "", value)
    print value
    count++
  }
  END { if (count != 1) exit 1 }
' "$manifest")" || {
  echo "Unable to read one [workspace.package].version from $manifest" >&2
  exit 1
}

if [[ ! $version =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "Workspace version is not a semantic version: $version" >&2
  exit 1
fi

printf '%s\n' "$version"
