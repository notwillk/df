#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
TOOLCHAIN_FILE="${ROOT_DIR}/rust-toolchain.toml"
CHANNEL="$(sed -n 's/^channel = "\(.*\)"/\1/p' "${TOOLCHAIN_FILE}")"

if ! command -v rustup >/dev/null 2>&1; then
  curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal --default-toolchain none
fi

export PATH="${HOME}/.cargo/bin:${PATH}"

rustup toolchain install "${CHANNEL}" --profile minimal
rustup component add --toolchain "${CHANNEL}" clippy rustfmt
