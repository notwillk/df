#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
TOOLCHAIN_FILE="${ROOT_DIR}/rust-toolchain.toml"
CHANNEL="$(sed -n 's/^channel = "\(.*\)"/\1/p' "${TOOLCHAIN_FILE}")"

if ! command -v rustup >/dev/null 2>&1; then
  exit 1
fi

if ! rustup toolchain list | grep -Fq "${CHANNEL}"; then
  exit 1
fi

for component in clippy rustfmt; do
  rustup component list --toolchain "${CHANNEL}" --installed | grep -Eq "^${component}(-|$)"
done
