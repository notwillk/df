#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
workspace_root="$(CDPATH= cd -- "$script_dir/../../../.." && pwd)"

bash -n \
  "$workspace_root/apps/cli/scripts/cross-compile.sh" \
  "$workspace_root/apps/cli/scripts/ensure-tag-matches-version.sh" \
  "$workspace_root/apps/cli/scripts/get-version.sh" \
  "$workspace_root/apps/cli/scripts/release.sh" \
  "$workspace_root/apps/cli/scripts/verify-macos-binary.sh" \
  "$workspace_root/apps/cli/scripts/verify-static-linux-binary.sh" \
  "$script_dir/all.sh" \
  "$script_dir/artifact-contract.sh" \
  "$script_dir/binary-safety-smoke.sh" \
  "$script_dir/release-contract.sh"
sh -n "$workspace_root/scripts/install.sh" "$workspace_root/scripts/tests/install.sh"

"$script_dir/release-contract.sh"
"$script_dir/artifact-contract.sh"
"$workspace_root/scripts/tests/install.sh"
